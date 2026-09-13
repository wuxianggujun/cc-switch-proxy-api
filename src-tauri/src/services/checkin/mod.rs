//! 公益站签到
//!
//! 每个站的签到接口形状都不一样，所以这里不做「预设面板类型」，
//! 而是把签到建模成一条用户可完整描述的 HTTP 请求：
//! 方法 / URL / 请求头 / 请求体 / 成功判定串。
//!
//! 认证有三条路径：
//! - `Header`：直接带 Cookie 或 Token（简单，但会过期）
//! - `Login`：先用账密登录拿 Set-Cookie，再带着 cookie 发签到请求
//! - `Browser`：每条条目独立浏览器 profile，分别读取账号 Cookie 与 Cloudflare 凭证
//!
//! `Browser` 是唯一能过 CF 的方式：CF 校验浏览器运行时与 TLS 指纹，
//! 伪装请求头无效。它同时避免把站点密码落库，安全性优于 `Login`。
//!
//! `Header` / `Login` 的凭证明文存在 SQLite 的 settings 表里（与仓库既有的
//! S3 secret_access_key 一致）。前端面板必须显式告知用户这一点。

pub mod browser;
pub mod executor;
pub mod profile;
pub mod runner;
pub mod scheduler;

use crate::error::AppError;
use crate::store::AppState;
use serde::{Deserialize, Serialize};

/// 认证方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckinAuthKind {
    /// 直接在请求头里带 Cookie / Authorization。
    Header,
    /// 先账密登录换取 session cookie。
    Login,
    /// 从条目独立 profile 读取账号 Cookie，必要时过 Cloudflare 挑战。
    Browser,
}

/// 请求体编码方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckinBodyKind {
    #[default]
    None,
    Json,
    Form,
}

/// 一条请求头键值对。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckinHeader {
    pub name: String,
    pub value: String,
}

/// 账密登录配置（`auth_kind == Login` 时使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinLogin {
    pub url: String,
    /// 登录表单里用户名字段的键名，各站不同（username / email / account）。
    #[serde(default = "default_username_field")]
    pub username_field: String,
    #[serde(default = "default_password_field")]
    pub password_field: String,
    pub username: String,
    pub password: String,
    #[serde(default = "default_json_body")]
    pub body_kind: CheckinBodyKind,
}

fn default_username_field() -> String {
    "username".to_string()
}

fn default_password_field() -> String {
    "password".to_string()
}

fn default_json_body() -> CheckinBodyKind {
    CheckinBodyKind::Json
}

/// 浏览器过闸配置（`auth_kind == Browser` 时使用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinBrowser {
    /// 用于过闸的页面地址。留空则回退到 `site_url`，再退到签到请求 URL。
    /// 单独可配是因为部分站点的 CF 挑战只在特定页面触发。
    #[serde(default)]
    pub challenge_url: String,
    /// 独立账号窗口的登录地址。留空使用 site_url，再退到签到 URL 的站点根地址。
    #[serde(default)]
    pub login_url: String,
    /// 上次过闸拿到的 cookie 与 UA，供下次直接复用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<CheckinClearance>,
}

/// 缓存的过闸凭证。cookie 与 UA 必须成对使用，缺一即失效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinClearance {
    /// 可直接用于 `Cookie` 头的串。
    pub cookie: String,
    /// 与 cookie 配对的 UA。
    pub user_agent: String,
    /// 获取时间，Unix 秒。用于判断是否该重新过闸。
    pub acquired_at: i64,
}

/// 过闸凭证的本地有效期。CF 的 cf_clearance 实际时长由站点配置决定，
/// 这里取一个保守值：过期就重新过闸，比拿着废 cookie 请求失败要好。
pub const CLEARANCE_TTL_SECS: i64 = 30 * 60;

impl CheckinClearance {
    /// 是否仍在本地 TTL 内。时钟回拨也算过期，避免拿着旧 cookie 死循环。
    pub fn is_fresh(&self, now: i64) -> bool {
        now >= self.acquired_at && now - self.acquired_at < CLEARANCE_TTL_SECS
    }
}

/// 签到请求本体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRequest {
    #[serde(default = "default_method")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<CheckinHeader>,
    #[serde(default)]
    pub body_kind: CheckinBodyKind,
    /// 原始请求体；body_kind 为 Json 时应是 JSON 文本，Form 时是 a=1&b=2。
    #[serde(default)]
    pub body: String,
    /// 成功判定：响应体里包含该子串才算成功。空则只看 HTTP 状态码。
    /// 必要是因为多数站点签到失败也返回 200，靠 body 里的 message 区分。
    #[serde(default)]
    pub success_contains: String,
}

fn default_method() -> String {
    "POST".to_string()
}

/// 单次签到结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckinStatus {
    /// 签到成功。
    Success,
    /// 请求成功送达但站点判定未签到成功（含「今日已签到」）。
    Failed,
    /// 网络层面失败：超时、DNS、连接被拒。
    Error,
    /// 被 Cloudflare 挑战拦截，拿到的是挑战页而非站点响应。
    /// 与 Failed 分开是因为处置不同：这个要重新过闸，不是「今天已签过」。
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResult {
    pub status: CheckinStatus,
    /// Unix 秒。
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 展示给用户的信息：站点返回的 message，或网络错误原因。
    #[serde(default)]
    pub message: String,
    /// 登录失效仍是 Failed，而不是 Cloudflare Blocked 或网络 Error。
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_login: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// 只向前端返回状态，不返回独立 profile 中的账号 Cookie。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinBrowserSessionStatus {
    pub account_cookie_count: usize,
    pub login_window_open: bool,
}

/// 一个公益站条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinSite {
    pub id: String,
    pub name: String,
    /// 站点主页，供前端「打开」按钮使用；不参与签到请求。
    #[serde(default)]
    pub site_url: String,
    pub auth_kind: CheckinAuthKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<CheckinLogin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<CheckinBrowser>,
    pub request: CheckinRequest,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub sort_index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<CheckinResult>,
}

fn default_true() -> bool {
    true
}

impl CheckinSite {
    pub fn resolve_login_url(&self) -> Result<url::Url, String> {
        if let Some(configured) = self
            .browser
            .as_ref()
            .map(|browser| browser.login_url.trim())
            .filter(|url| !url.is_empty())
        {
            return parse_browser_url(configured);
        }
        if !self.site_url.trim().is_empty() {
            return parse_browser_url(&self.site_url);
        }
        let mut url = parse_browser_url(&self.request.url)?;
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    /// 过闸目标页：优先 `browser.challenge_url`，其次站点主页，最后签到请求 URL。
    ///
    /// 回退到请求 URL 是保底——CF 挑战通常在页面导航时触发，而签到接口多为
    /// POST API，未必会返回挑战页；所以更推荐用户显式填 challenge_url 或 site_url。
    pub fn resolve_challenge_url(&self) -> &str {
        if let Some(browser) = self.browser.as_ref() {
            let configured = browser.challenge_url.trim();
            if !configured.is_empty() {
                return configured;
            }
        }
        let site = self.site_url.trim();
        if !site.is_empty() {
            return site;
        }
        self.request.url.trim()
    }

    /// 取仍在 TTL 内的过闸凭证。
    pub fn fresh_clearance(&self, now: i64) -> Option<&CheckinClearance> {
        self.browser
            .as_ref()
            .and_then(|b| b.cached.as_ref())
            .filter(|c| c.is_fresh(now))
    }
}

/// 全局签到配置（含站点列表与调度设置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinConfig {
    #[serde(default)]
    pub sites: Vec<CheckinSite>,
    /// 每日自动签到总开关。
    #[serde(default)]
    pub schedule_enabled: bool,
    /// 每日执行的小时（0-23，本地时区）。
    #[serde(default = "default_schedule_hour")]
    pub schedule_hour: u8,
    /// 最近一次全量执行的本地日期（YYYY-MM-DD），用于当天去重。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_date: Option<String>,
}

fn default_schedule_hour() -> u8 {
    9
}

impl Default for CheckinConfig {
    fn default() -> Self {
        Self {
            sites: Vec::new(),
            schedule_enabled: false,
            schedule_hour: default_schedule_hour(),
            last_run_date: None,
        }
    }
}

/// settings 表里的存储键。复用键值表而非新建表，避免占用 SCHEMA_VERSION。
const CHECKIN_CONFIG_KEY: &str = "checkin_config";

pub struct CheckinService;

impl CheckinService {
    pub fn load(state: &AppState) -> Result<CheckinConfig, AppError> {
        let raw = state.db.get_setting(CHECKIN_CONFIG_KEY)?;
        match raw {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Config(format!("解析签到配置失败: {e}"))),
            None => Ok(CheckinConfig::default()),
        }
    }

    pub fn save(state: &AppState, config: &CheckinConfig) -> Result<(), AppError> {
        let json = serde_json::to_string(config)
            .map_err(|e| AppError::Config(format!("序列化签到配置失败: {e}")))?;
        state.db.set_setting(CHECKIN_CONFIG_KEY, &json)
    }

    /// 新增或更新一个站点。`last_result` 由执行流程维护，这里保留既有值，
    /// 避免前端提交表单时把历史结果清掉。
    pub fn upsert_site(state: &AppState, mut site: CheckinSite) -> Result<CheckinSite, AppError> {
        Self::validate(&site)?;
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;

        if site.id.trim().is_empty() {
            site.id = uuid::Uuid::new_v4().to_string();
        }

        // 缓存仅由后端写入。条目或目标 origin 变化后不能沿用旧站点的凭证。
        let cached = config
            .sites
            .iter()
            .find(|previous| same_browser_binding(previous, &site))
            .and_then(|previous| previous.browser.as_ref())
            .and_then(|browser| browser.cached.clone());
        if site.auth_kind == CheckinAuthKind::Browser {
            site.browser
                .get_or_insert_with(CheckinBrowser::default)
                .cached = cached;
        } else {
            site.browser = None;
        }

        match config.sites.iter().position(|s| s.id == site.id) {
            Some(index) => {
                site.last_result = config.sites[index].last_result.clone();
                site.sort_index = config.sites[index].sort_index;
                config.sites[index] = site.clone();
            }
            None => {
                site.sort_index = config
                    .sites
                    .iter()
                    .map(|s| s.sort_index)
                    .max()
                    .map_or(0, |max| max + 1);
                config.sites.push(site.clone());
            }
        }

        Self::save(state, &config)?;
        Ok(site)
    }

    pub fn delete_site(state: &AppState, id: &str) -> Result<bool, AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        let before = config.sites.len();
        config.sites.retain(|s| s.id != id);
        let removed = config.sites.len() != before;
        if removed {
            Self::save(state, &config)?;
        }
        Ok(removed)
    }

    pub fn set_schedule(
        state: &AppState,
        schedule_enabled: bool,
        schedule_hour: u8,
    ) -> Result<CheckinConfig, AppError> {
        if schedule_hour > 23 {
            return Err(AppError::InvalidInput(format!(
                "签到时间必须在 0-23 之间，收到 {schedule_hour}"
            )));
        }
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        config.schedule_enabled = schedule_enabled;
        config.schedule_hour = schedule_hour;
        Self::save(state, &config)?;
        Ok(config)
    }

    /// 写回过闸凭证，供下次签到复用。站点可能已被并发删除，静默跳过。
    #[allow(dead_code)]
    pub fn record_clearance(
        state: &AppState,
        id: &str,
        clearance: CheckinClearance,
    ) -> Result<(), AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        if let Some(site) = config
            .sites
            .iter_mut()
            .find(|s| s.id == id && s.auth_kind == CheckinAuthKind::Browser)
        {
            site.browser
                .get_or_insert_with(CheckinBrowser::default)
                .cached = Some(clearance);
            Self::save(state, &config)?;
        }
        Ok(())
    }

    /// Do not attach an in-flight browser result to an entry whose origin/auth changed.
    pub(super) fn record_clearance_if_current(
        state: &AppState,
        expected: &CheckinSite,
        clearance: CheckinClearance,
    ) -> Result<bool, AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        let Some(site) = config
            .sites
            .iter_mut()
            .find(|site| same_browser_binding(site, expected))
        else {
            return Ok(false);
        };
        site.browser
            .get_or_insert_with(CheckinBrowser::default)
            .cached = Some(clearance);
        Self::save(state, &config)?;
        Ok(true)
    }

    /// 丢弃过闸凭证。签到被判定为遭 CF 拦截时调用，下次强制重新过闸。
    pub fn clear_clearance(state: &AppState, id: &str) -> Result<(), AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        if let Some(site) = config.sites.iter_mut().find(|s| s.id == id) {
            if let Some(browser) = site.browser.as_mut() {
                if browser.cached.is_some() {
                    browser.cached = None;
                    Self::save(state, &config)?;
                }
            }
        }
        Ok(())
    }

    /// 写回一次执行结果。站点可能已被并发删除，此时静默跳过。
    pub fn record_result(
        state: &AppState,
        id: &str,
        result: CheckinResult,
    ) -> Result<(), AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        if let Some(site) = config.sites.iter_mut().find(|s| s.id == id) {
            site.last_result = Some(result);
            Self::save(state, &config)?;
        }
        Ok(())
    }

    pub(crate) fn complete_scheduled_run(state: &AppState, date: String) -> Result<(), AppError> {
        let _guard = lock_config(state)?;
        let mut config = Self::load(state)?;
        config.last_run_date = Some(date);
        Self::save(state, &config)
    }

    fn validate(site: &CheckinSite) -> Result<(), AppError> {
        if site.name.trim().is_empty() {
            return Err(AppError::InvalidInput("站点名称不能为空".into()));
        }
        if site.request.url.trim().is_empty() {
            return Err(AppError::InvalidInput("签到请求 URL 不能为空".into()));
        }
        if !is_http_url(&site.request.url) {
            return Err(AppError::InvalidInput(
                "签到请求 URL 必须以 http:// 或 https:// 开头".into(),
            ));
        }

        if site.auth_kind == CheckinAuthKind::Browser {
            site.resolve_login_url().map_err(AppError::InvalidInput)?;
            // challenge_url 可留空（回退到 site_url / 请求 URL），但填了就必须合法。
            if let Some(browser) = site.browser.as_ref() {
                let raw = browser.challenge_url.trim();
                if !raw.is_empty() && !is_http_url(raw) {
                    return Err(AppError::InvalidInput(
                        "验证页面 URL 必须以 http:// 或 https:// 开头".into(),
                    ));
                }
            }
        }

        if site.auth_kind == CheckinAuthKind::Login {
            let login = site
                .login
                .as_ref()
                .ok_or_else(|| AppError::InvalidInput("选择账密登录时必须填写登录配置".into()))?;
            if !is_http_url(&login.url) {
                return Err(AppError::InvalidInput(
                    "登录 URL 必须以 http:// 或 https:// 开头".into(),
                ));
            }
            if login.username.trim().is_empty() || login.password.is_empty() {
                return Err(AppError::InvalidInput("登录账号和密码不能为空".into()));
            }
        }

        Ok(())
    }
}

fn is_http_url(url: &str) -> bool {
    url::Url::parse(url.trim()).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub(super) fn parse_browser_url(raw: &str) -> Result<url::Url, String> {
    if !is_http_url(raw) {
        return Err("浏览器页面必须是有效的 HTTP/HTTPS URL，且不能包含用户名或密码".into());
    }
    url::Url::parse(raw.trim()).map_err(|_| "浏览器页面 URL 无效".into())
}

pub(super) fn same_browser_binding(left: &CheckinSite, right: &CheckinSite) -> bool {
    let same_origin = |left: &str, right: &str| {
        parse_browser_url(left)
            .ok()
            .zip(parse_browser_url(right).ok())
            .is_some_and(|(left, right)| left.origin() == right.origin())
    };
    left.id == right.id
        && left.auth_kind == CheckinAuthKind::Browser
        && right.auth_kind == CheckinAuthKind::Browser
        && same_origin(&left.request.url, &right.request.url)
        && same_origin(left.resolve_challenge_url(), right.resolve_challenge_url())
        && left
            .resolve_login_url()
            .ok()
            .zip(right.resolve_login_url().ok())
            .is_some_and(|(left, right)| left.origin() == right.origin())
}

fn lock_config(state: &AppState) -> Result<std::sync::MutexGuard<'_, ()>, AppError> {
    state
        .checkin_runtime
        .config_write
        .lock()
        .map_err(|_| AppError::Config("签到配置写入锁不可用".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::sync::Arc;

    #[test]
    fn readiness_default_schedule_hour_is_nine() {
        assert_eq!(CheckinConfig::default().schedule_hour, 9);
        assert_eq!(
            serde_json::from_str::<CheckinConfig>("{}")
                .unwrap()
                .schedule_hour,
            9
        );
    }

    fn browser_site(challenge_url: &str, site_url: &str, request_url: &str) -> CheckinSite {
        CheckinSite {
            id: "site-1".to_string(),
            name: "测试站".to_string(),
            site_url: site_url.to_string(),
            auth_kind: CheckinAuthKind::Browser,
            login: None,
            browser: Some(CheckinBrowser {
                challenge_url: challenge_url.to_string(),
                login_url: String::new(),
                cached: None,
            }),
            request: CheckinRequest {
                method: "POST".to_string(),
                url: request_url.to_string(),
                headers: Vec::new(),
                body_kind: CheckinBodyKind::None,
                body: String::new(),
                success_contains: String::new(),
            },
            enabled: true,
            sort_index: 0,
            last_result: None,
        }
    }

    fn clearance(acquired_at: i64) -> CheckinClearance {
        CheckinClearance {
            cookie: "cf_clearance=token".to_string(),
            user_agent: browser::CLEARANCE_USER_AGENT.to_string(),
            acquired_at,
        }
    }

    #[test]
    fn challenge_url_falls_back_through_site_then_request() {
        let full = browser_site(
            "https://a.example/verify",
            "https://b.example",
            "https://c.example/api/checkin",
        );
        assert_eq!(full.resolve_challenge_url(), "https://a.example/verify");

        // 空白也算未配置，否则会拿 "  " 去 parse URL 失败。
        let no_challenge =
            browser_site("   ", "https://b.example", "https://c.example/api/checkin");
        assert_eq!(no_challenge.resolve_challenge_url(), "https://b.example");

        let only_request = browser_site("", "", "https://c.example/api/checkin");
        assert_eq!(
            only_request.resolve_challenge_url(),
            "https://c.example/api/checkin"
        );
    }

    #[test]
    fn clearance_freshness_covers_ttl_edges_and_clock_skew() {
        let now = 1_700_000_000;
        assert!(clearance(now).is_fresh(now));
        assert!(clearance(now - CLEARANCE_TTL_SECS + 1).is_fresh(now));
        // 正好到 TTL 即过期，避免边界上拿着将死的 cookie 发请求。
        assert!(!clearance(now - CLEARANCE_TTL_SECS).is_fresh(now));
        // 时钟回拨（acquired_at 在未来）也算过期，否则会一直复用旧 cookie。
        assert!(!clearance(now + 60).is_fresh(now));
    }

    #[test]
    fn fresh_clearance_requires_browser_config_and_cache() {
        let now = 1_700_000_000;

        let mut site = browser_site("https://a.example", "", "https://c.example/api");
        assert!(site.fresh_clearance(now).is_none());

        site.browser.as_mut().expect("browser").cached = Some(clearance(now));
        assert!(site.fresh_clearance(now).is_some());

        site.browser.as_mut().expect("browser").cached = Some(clearance(now - CLEARANCE_TTL_SECS));
        assert!(site.fresh_clearance(now).is_none());

        site.browser = None;
        assert!(site.fresh_clearance(now).is_none());
    }

    #[test]
    fn upsert_preserves_cached_clearance_when_form_omits_it() {
        let state = AppState::new(Arc::new(Database::memory().expect("in-memory database")));

        let mut site = browser_site("https://a.example", "", "https://c.example/api");
        site.id = String::new();
        let saved = CheckinService::upsert_site(&state, site).expect("insert site");

        let acquired_at = 1_700_000_000;
        CheckinService::record_clearance(&state, &saved.id, clearance(acquired_at))
            .expect("record clearance");

        // 模拟前端提交表单：带 browser 但 cached 为 None，不能把凭证冲掉。
        let mut edited = saved.clone();
        edited.name = "改名后".to_string();
        edited.browser = Some(CheckinBrowser {
            challenge_url: "https://a.example/verify".to_string(),
            login_url: String::new(),
            cached: None,
        });
        CheckinService::upsert_site(&state, edited).expect("update site");

        let config = CheckinService::load(&state).expect("load config");
        let stored = &config.sites[0];
        assert_eq!(stored.name, "改名后");
        assert_eq!(
            stored.browser.as_ref().expect("browser").challenge_url,
            "https://a.example/verify"
        );
        let cached = stored
            .browser
            .as_ref()
            .and_then(|b| b.cached.as_ref())
            .expect("cached clearance survives form submit");
        assert_eq!(cached.acquired_at, acquired_at);
    }

    #[test]
    fn clear_clearance_forces_reacquisition() {
        let state = AppState::new(Arc::new(Database::memory().expect("in-memory database")));

        let mut site = browser_site("https://a.example", "", "https://c.example/api");
        site.id = String::new();
        let saved = CheckinService::upsert_site(&state, site).expect("insert site");
        CheckinService::record_clearance(&state, &saved.id, clearance(1_700_000_000))
            .expect("record clearance");

        CheckinService::clear_clearance(&state, &saved.id).expect("clear clearance");

        let config = CheckinService::load(&state).expect("load config");
        assert!(config.sites[0]
            .browser
            .as_ref()
            .expect("browser")
            .cached
            .is_none());
    }

    #[test]
    fn validate_rejects_malformed_challenge_url_but_allows_empty() {
        let ok = browser_site("", "https://b.example", "https://c.example/api");
        assert!(CheckinService::validate(&ok).is_ok());

        let bad = browser_site("example.com/verify", "", "https://c.example/api");
        assert!(CheckinService::validate(&bad).is_err());
    }

    #[test]
    fn profile_b_changing_request_origin_invalidates_cached_clearance() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let saved = CheckinService::upsert_site(
            &state,
            browser_site("https://a.example", "", "https://a.example/checkin"),
        )
        .unwrap();
        CheckinService::record_clearance(&state, &saved.id, clearance(1_700_000_000)).unwrap();
        let mut edited = saved;
        edited.request.url = "https://b.example/checkin".into();
        let updated = CheckinService::upsert_site(&state, edited).unwrap();
        assert!(updated.browser.unwrap().cached.is_none());
    }

    #[test]
    fn profile_b_imported_clearance_cannot_select_another_entry_session() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let mut imported = browser_site("https://a.example", "", "https://a.example/checkin");
        imported.browser.as_mut().unwrap().cached = Some(clearance(1_700_000_000));
        let saved = CheckinService::upsert_site(&state, imported).unwrap();
        assert!(saved.browser.unwrap().cached.is_none());
    }

    #[test]
    fn profile_b_login_url_roundtrips_through_saved_configuration() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let mut value = serde_json::to_value(browser_site(
            "https://a.example",
            "",
            "https://a.example/checkin",
        ))
        .unwrap();
        value["browser"]["loginUrl"] = serde_json::json!("https://a.example/login");
        let saved =
            CheckinService::upsert_site(&state, serde_json::from_value(value).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(saved).unwrap()["browser"]["loginUrl"],
            "https://a.example/login"
        );
    }

    #[test]
    fn profile_b_login_url_defaults_to_site_root_without_api_path_or_query() {
        let mut site = browser_site(
            "",
            "",
            "https://same.example/api/checkin?token=secret#fragment",
        );
        assert_eq!(
            site.resolve_login_url().unwrap().as_str(),
            "https://same.example/"
        );
        site.site_url = "https://same.example/dashboard".into();
        assert_eq!(
            site.resolve_login_url().unwrap().as_str(),
            "https://same.example/dashboard"
        );
        site.browser.as_mut().unwrap().login_url = "https://same.example/login".into();
        assert_eq!(
            site.resolve_login_url().unwrap().as_str(),
            "https://same.example/login"
        );
        site.browser.as_mut().unwrap().login_url = "https://user:password@same.example/".into();
        assert!(CheckinService::validate(&site).is_err());
    }

    #[test]
    fn profile_b_late_clearance_cannot_repopulate_an_edited_or_deleted_entry() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let saved = CheckinService::upsert_site(
            &state,
            browser_site("https://a.example", "", "https://a.example/api"),
        )
        .unwrap();
        let mut edited = saved.clone();
        edited.auth_kind = CheckinAuthKind::Header;
        CheckinService::upsert_site(&state, edited).unwrap();
        assert!(
            !CheckinService::record_clearance_if_current(&state, &saved, clearance(1)).unwrap()
        );
        assert!(CheckinService::load(&state).unwrap().sites[0]
            .browser
            .is_none());
        CheckinService::delete_site(&state, &saved.id).unwrap();
        assert!(
            !CheckinService::record_clearance_if_current(&state, &saved, clearance(1)).unwrap()
        );
    }
}
