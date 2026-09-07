/**
 * 公益站签到类型。与 src-tauri/src/services/checkin/mod.rs 的 serde
 * 结构一一对应（Rust 侧统一 camelCase）。
 */

/**
 * 认证方式：直接带凭证头、先账密登录换 cookie，或开真实 WebView
 * 过 Cloudflare 挑战。browser 是唯一能过 CF 的方式。
 */
export type CheckinAuthKind = "header" | "login" | "browser";

/** 请求体编码方式。 */
export type CheckinBodyKind = "none" | "json" | "form";

/**
 * 签到结果状态。error 是网络层失败，failed 是站点判定未成功，
 * blocked 是被 Cloudflare 拦截（需重新过闸，不同于「今天已签过」）。
 */
export type CheckinStatus = "success" | "failed" | "error" | "blocked";

export interface CheckinHeader {
  name: string;
  value: string;
}

export interface CheckinLogin {
  url: string;
  /** 登录表单里用户名字段的键名，各站不同（username / email / account）。 */
  usernameField: string;
  passwordField: string;
  username: string;
  password: string;
  bodyKind: CheckinBodyKind;
}

/** 缓存的过闸凭证。cookie 与 UA 必须成对使用，缺一即失效。 */
export interface CheckinClearance {
  cookie: string;
  userAgent: string;
  /** 获取时间，Unix 秒。 */
  acquiredAt: number;
}

/** 浏览器过闸配置（authKind 为 browser 时使用）。 */
export interface CheckinBrowser {
  /** 过闸页面地址。留空则回退到 siteUrl，再退到签到请求 URL。 */
  challengeUrl: string;
  /** 上次过闸结果，由后端维护，前端提交表单时无需携带。 */
  cached?: CheckinClearance;
}

/**
 * 过闸凭证有效期，秒。必须与 services/checkin/mod.rs 的
 * CLEARANCE_TTL_SECS 保持一致 —— 此处仅用于界面展示，
 * 真正的过期判定在后端。
 */
export const CLEARANCE_TTL_SECS = 30 * 60;

/** 凭证是否仍在有效期内。与后端 CheckinClearance::is_fresh 同逻辑。 */
export function isClearanceFresh(clearance: CheckinClearance): boolean {
  const now = Math.floor(Date.now() / 1000);
  return (
    now >= clearance.acquiredAt &&
    now - clearance.acquiredAt < CLEARANCE_TTL_SECS
  );
}

export interface CheckinRequest {
  method: string;
  url: string;
  headers: CheckinHeader[];
  bodyKind: CheckinBodyKind;
  body: string;
  /** 响应体含该子串才算成功。空则只看 HTTP 状态码。 */
  successContains: string;
}

export interface CheckinResult {
  status: CheckinStatus;
  /** Unix 秒。 */
  at: number;
  httpStatus?: number;
  message: string;
}

export interface CheckinSite {
  id: string;
  name: string;
  /** 站点主页，仅供「打开」按钮使用。 */
  siteUrl: string;
  authKind: CheckinAuthKind;
  login?: CheckinLogin;
  browser?: CheckinBrowser;
  request: CheckinRequest;
  enabled: boolean;
  sortIndex: number;
  lastResult?: CheckinResult;
}

export interface CheckinConfig {
  sites: CheckinSite[];
  scheduleEnabled: boolean;
  /** 每日执行的小时，0-23，本地时区。 */
  scheduleHour: number;
  /** 最近一次全量执行的本地日期 YYYY-MM-DD。 */
  lastRunDate?: string;
}

/** 新建站点时的空白模板。 */
export function emptyCheckinSite(): CheckinSite {
  return {
    id: "",
    name: "",
    siteUrl: "",
    authKind: "header",
    request: {
      method: "POST",
      url: "",
      headers: [],
      bodyKind: "none",
      body: "",
      successContains: "",
    },
    enabled: true,
    sortIndex: 0,
  };
}

export function emptyCheckinBrowser(): CheckinBrowser {
  return { challengeUrl: "" };
}

export function emptyCheckinLogin(): CheckinLogin {
  return {
    url: "",
    usernameField: "username",
    passwordField: "password",
    username: "",
    password: "",
    bodyKind: "json",
  };
}
