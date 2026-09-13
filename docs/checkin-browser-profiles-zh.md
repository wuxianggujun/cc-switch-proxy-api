# 自动签到独立浏览器账号（方案 B）

## 使用

1. 为同一站点的不同账号分别创建条目，认证方式选择“浏览器验证”。填写站点主页、签到请求，必要时填写账号登录页面，然后保存。
2. 在条目列表或编辑框中点击“打开登录窗口”。窗口标题包含当前条目名称；每个条目只管理自己的账号。
3. 在窗口内完成登录，再关闭该窗口。关闭账号窗口不会退出应用，也不会关闭其它账号窗口。账号状态只显示目标站点 Cookie 的数量，不把 Cookie 值暴露到前端；存在 Cookie 不等于服务端仍认可登录态。
4. 手动执行一次签到验证返回结果，再启用每日自动签到。登录过期时结果仍是 `failed`，显示“需登录”，可重新打开对应条目的登录窗口。
5. 未保存的新条目或已修改页面地址的表单，不能操作旧地址对应的浏览器窗口，必须先保存。

Cookie 登录的网站可自动复用账号状态；只把 Token 放在 localStorage 的网站仍需显式配置 `Authorization` 等请求头。本实现不猜测任意站点的 JavaScript 登录协议，也不注入脚本窃取或导出 localStorage。

## 模块和数据流

| 模块                                            | 职责                                                                   |
| ----------------------------------------------- | ---------------------------------------------------------------------- |
| `services/checkin/profile.rs`                   | profile 标识、目录校验、窗口/弹窗生命周期、目标 URL 的账号 Cookie 读取 |
| `services/checkin/browser.rs`                   | 在该条目的 profile 内过 Cloudflare，只导出 `cf_clearance`              |
| `services/checkin/runner.rs`                    | 组装账号 Cookie、过闸凭证、显式请求头并发送请求，判定业务结果          |
| `services/checkin/executor.rs`                  | 手动/自动签到统一编排、缓存更新、过闸重试、账号窗口互斥                |
| `commands/checkin.rs`                           | IPC 参数转发，不将账号 Cookie 返回前端                                 |
| `CheckinBrowserAccountControls`、签到表单与列表 | 独立登录入口、账号状态刷新和重新登录提示                               |

数据流：条目 ID → 对应 profile → `cookies_for_url(签到请求 URL)` → 账号 Cookie 与 CF Cookie 分离 → 用户显式头覆盖同名 Cookie → reqwest → 签到结果。

- profile 目录：`<get_app_config_dir()>/checkin/profiles/<SHA-256>`。目录名由稳定的条目 ID 与签到 URL origin 共同计算，重命名或修改同源接口路径不会更换 profile，不同账号或不同 origin 必定分离。导入的 ID 不能作为文件路径使用。
- 登录窗口、过闸窗口、Cookie 读取窗口和登录弹窗统一使用同一个窗口工厂，显式设置 `data_directory` 和原有 `CLEARANCE_USER_AGENT`。
- Windows 只使用普通绝对路径作为 WebView2 数据目录。`canonicalize()` 的 `\\?\` 路径仅用于安全校验，不能传给 Chromium 的 Cookie SQLite 存储，否则 Cookie 可能只能留在内存。
- 普通关闭会隐藏账号主窗口，保留应用运行期间的 session Cookie。网站设置的持久 Cookie 由 WebView2 profile 自身存储；浏览器或站点定义为会话期/已过期的凭证不保证跨进程继续有效。
- 登录弹窗沿用该条目 profile，不回退到主窗口 Cookie 存储。账号主窗口隐藏后不再接受其弹窗请求，现有子窗口在原生事件回调外排队关闭。初始化中的弹窗会先隐藏，等首次真实导航确认 WebView2 已完成 `SetNewWindow` 绑定后再关闭，避免空指针崩溃。
- 登录窗口的打开状态由应用在 `show`、关闭和销毁事件中显式维护，不依赖刚启动的 WebView2 响应 `is_visible()`。新建隐藏 WebView 后读取 CookieManager 使用最多约 3 秒的有限重试；超时仍按执行错误返回，不无限等待或吞掉错误。
- 删除条目或改变认证/origin 会关闭原账号窗口。不会递归删除用户 profile 文件，也不会从旧共享浏览器自动搬运 session；新条目 ID 不会复用被删除条目的目录。

## 凭证和状态规则

1. `join_cookies` 保留方案 A 的严格白名单，只导出 `cf_clearance`。旧版完整 Cookie 缓存也会过滤，不能成为账号凭证来源。
2. 账号 Cookie 单独从该条目的实际请求 URL 读取，使用 WebView 的 HttpOnly、Secure、Domain 和 Path 匹配规则，不读取所有站点的 Cookie。不额外复制到 SQLite，也不传入前端状态接口。
3. 同名 Cookie 优先级：用户显式请求头 > 当前条目账号 Cookie > CF 过闸凭证。最终只生成一个 Cookie 请求头；UA 仍强制匹配过闸窗口。
4. 普通站点先带账号 Cookie 直接签到，不等待不存在的 `cf_clearance`。只有收到真实 Cloudflare 拦截才重新过闸并重试一次，过闸后再读取最新账号 Cookie。
5. 强制重新验证会清除该 profile 中已被拒绝的 `cf_clearance`，不会删除账号 Cookie；避免重新过闸时立即拾取同一个已失效凭证。
6. `blocked` 仅用于 Cloudflare；`failed` 仍表示业务失败；`error` 表示网络或执行失败。新增可选字段 `needsLogin`，将 HTTP 401、明确登录失效 JSON、跳转到登录页的响应指向重新登录，不将普通 403 或“今日已签到”错误标成登录失效。
7. 手动签到、调度、账号状态读取和配置更新共用后端执行锁。登录窗口仍打开时暂停该条目签到，定时任务在窗口关闭后恢复，并跳过当日已成功的其它条目。
8. 改变目标 origin 或认证方式时清除旧缓存。表单/导入请求不能写入内部过闸缓存，在途验证结果也不会写回已变更或已删除的条目。

签到客户端未接入代理池；`cf_clearance` 仍要求 UA 和出口 IP 与浏览器一致。系统网络环境或代理发生变化后，站点可能要求重新验证。

## 接口与兼容

- `open_checkin_login_window({ id }) -> void`：打开/聚焦对应条目的独立账号窗口。
- `get_checkin_browser_session_status({ id }) -> { accountCookieCount, loginWindowOpen }`：只返回元数据。
- `checkin-browser-updated` 事件只携带条目 ID；前端也可主动刷新状态。
- `browser.loginUrl` 为可选字段，旧配置无需迁移。留空时使用站点主页，再回退到签到 URL 的站点根地址，不把 API 路径和查询参数当成登录页面。
- macOS 采用同一身份生成 `data_store_identifier`，需要 macOS 14+；旧版本明确返回不支持，不静默回退到共享账号。Windows 使用 WebView2，Linux 使用 WebKit 的独立数据目录。原生验证范围以本机 Windows 为准。

## 可重复验证

在仓库根目录的 Git Bash 中执行；测试使用独立配置目录和虚构账号，不执行真实网站签到。

```bash
export PATH="$HOME/.cargo/bin:$PATH"
test_home=$(mktemp -d /tmp/cc-switch-profile-b.XXXXXX)
export CC_SWITCH_TEST_HOME="$(cygpath -m "$test_home")"
export NO_PROXY=127.0.0.1,localhost,::1
export no_proxy="$NO_PROXY"
export CARGO_INCREMENTAL=0

# 全部 Rust 库测试
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked \
  --config 'profile.test.package.cc-switch-proxy.debug=0' \
  -- --test-threads=1 --format=terse

# Windows 原生 WebView2 测试，需要桌面会话，会短暂显示本机测试窗口
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked \
  --config 'profile.test.package.cc-switch-proxy.debug=0' \
  profile_b_native_webview2_isolation_and_persistence \
  -- --ignored --test-threads=1 --nocapture

pnpm typecheck
pnpm test:unit --maxWorkers=2 --minWorkers=1
```

新增回归先在原实现上运行：4 个 Rust 用例失败，退出码 `101`；4 个前端用例失败，退出码 `1`。修复未修改期望值。新增原生测试覆盖真实 WebView2 同站点 A/B 账号、HttpOnly/Path Cookie、同 profile 弹窗、独立 CF 凭证、实际 HTTP 签到与窗口重建后的持久化。

### 本轮结果

| 验证                      | 结果                                                                           | 真实退出码 |
| ------------------------- | ------------------------------------------------------------------------------ | ---------- |
| Rust 先失败回归           | 原有 26 通过，新增 4 失败                                                      | 101        |
| 前端先失败回归            | 新增 4 个表单用例失败                                                          | 1          |
| 签到后端首轮              | 42 通过                                                                        | 0          |
| 真实 Windows WebView2（同进程） | 最终源码连续 3 次通过；覆盖 A/B 隔离、弹窗、过闸、HTTP 请求与窗口重建          | 每次 0     |
| 真实 Windows WebView2（全新进程） | 最终源码连续 3 次由全新测试进程直接读取 A/B 磁盘 Cookie，未重新登录            | 每次 0     |
| 全部 Rust 库测试          | 2930 通过、0 失败、7 跳过                                                      | 0          |
| 前端全量测试              | 139 个文件、1095 个用例通过                                                    | 0          |
| TypeScript 与签到前端专项 | 类型检查通过，27 个专项用例通过                                                | 0          |
| Playwright 表单验证       | 4 种语言 × 桌面/390px 窄屏，未发现文字溢出或脚本异常；点击登录传入 `account-b` | 通过       |

7 个 Rust 跳过用例中包括新加的桌面原生测试，它已通过单独的 `--ignored` 命令真实执行，并非未验证。

原生测试期间额外修复两个实际问题：Windows verbatim 路径造成 Cookie 不持久化，以及过早关闭新弹窗触发 WebView2 原生访问冲突。后者使用 Windows 调试器确认调用栈位于 `CheckUsingSameProfile` / `NewWindowRequestedEventArgs::put_NewWindow`，修复的是源码生命周期，未通过修改断言或增加固定等待绕过。

原始日志：

- Rust 失败回归：`C:/Users/wuxianggujun/.fastctx/jobs/j-cmr547/output.log`
- 表单失败回归：`C:/Users/wuxianggujun/.fastctx/jobs/j-y3ogty/output.log`
- 原生连续 5 次及 Rust 全量：`C:/Users/wuxianggujun/.fastctx/jobs/j-i5c8b0/output.log`
- 最终源码首次全新进程验证：`C:/Users/wuxianggujun/.fastctx/jobs/j-dthjib/output.log`
- 前端全量：`C:/Users/wuxianggujun/.fastctx/jobs/j-ixfb1n/output.log`
- TypeScript 与前端专项：`C:/Users/wuxianggujun/.fastctx/jobs/j-dfl8u5/output.log`

源码、JSON、测试和文档均为 UTF-8；语言资源使用严格 UTF-8 解码与 JSON 解析检查，不经 ANSI/GBK 转码。

本轮不接管安装包制作或替换正在运行的安装版。其它任务的包名、应用目录、端口与发布配置修改保持原状，以上路径始终通过项目当前配置目录 API 获取。
