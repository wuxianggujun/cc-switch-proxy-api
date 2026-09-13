# Browser 自动签到账号串号修复（方案 A）

后续已继续实现独立浏览器账号，当前使用方式与验证见 [方案 B](./checkin-browser-profiles-zh.md)。本文保留方案 A 的历史修复记录。

## 范围与根因

本轮只修复 browser 认证的 Cookie 边界，不实现独立浏览器 profile。

同一站点的验证窗口未设置独立 `data_directory`，会共用 WebView Cookie 存储。原 `join_cookies` 导出全部同域 Cookie，把浏览器账号 A 的 session 一起缓存；`send_checkin` 又在用户请求头之后合并缓存 Cookie，覆盖了用户为账号 B 显式填写的 session。

## 修复

1. `src-tauri/src/services/checkin/browser.rs`：`join_cookies` 只允许名称严格等于 `cf_clearance` 的 Cookie，排除 session、auth token、`__cf_bm` 等其它 Cookie。
2. `src-tauri/src/services/checkin/runner.rs`：先合并自动凭证，再合并用户请求头，同名 Cookie 以用户显式值为准，最终仍只发送一个 Cookie 头。
3. Browser 凭证读取处再次白名单过滤，兼容旧版已经保存的完整 Cookie 缓存。即使旧缓存尚未过期，也不会带出其中的账号登录态；不改动真实用户配置或执行数据库迁移。
4. `CheckinSiteFormModal` 继续使用 `checkin.form.browserHint`，同步更新 `en`、`zh`、`ja`、`zh-TW`：browser 仅处理 Cloudflare 验证，账号登录态必须在请求头中手动填写 Cookie，同站点各账号分别配置。

### 保持不变

- `CLEARANCE_USER_AGENT` 的值及 WebView/reqwest 配套 UA 规则。
- 签到 HTTP 客户端和出口路径，不接入代理池。`cf_clearance` 仍需匹配验证时的 UA 与出口 IP。
- Cloudflare 拦截为 `Blocked`，站点判定失败为 `Failed`，网络/执行错误为 `Error`；重试与调度逻辑不变。
- WebView 窗口创建逻辑、账号配置结构、缓存有效期和数据库结构。

## 先失败再修复

先只添加测试，运行原实现，之后才修改生产代码及语言资源；修复时没有改变测试断言或期望值。

真实回环 HTTP 服务捕获请求，明确复现：

```text
用户 Cookie：session=account-b; token=manual==
模拟 WebView Cookie：cf_clearance=passed; session=account-a
断言：实际收到的 session 必须为 account-b
修复前实际结果：account-a
cargo test 退出码：101
```

新增 Rust 回归用例：

- `browser_cookie_export_only_contains_cf_clearance`：只导出过闸 Cookie，保留值中的 `=`，Cookie 名区分大小写。
- `browser_cookie_export_without_clearance_is_empty`：无过闸 Cookie 时不能导出账号 session。
- `browser_cookie_does_not_override_explicit_account_cookie`：真实请求保留账号 B，保留过闸 Cookie，不产生重复 Cookie 头，并强制配套 UA。
- `browser_explicit_clearance_cookie_overrides_cached_clearance`：用户显式填写的同名 `cf_clearance` 也有最高优先级。
- `browser_cached_cookies_never_supply_account_login_state`：同站点账号 A、B 以及未填写账号三种情况共用旧版缓存，账号身份只能来自各自显式请求头。

新增四语言提示测试。旧文案均未说明 Cookie，因此修改前四个用例均失败，前端测试退出码为 `1`。

### 本轮已完成的验证结果

| 检查                                 | 结果                                            | 真实退出码 |
| ------------------------------------ | ----------------------------------------------- | ---------- |
| Rust 修复前签到回归                  | 21 通过，5 个新增用例按预期失败                 | 101        |
| Rust 修复后签到回归                  | 26 通过，0 失败                                 | 0          |
| 全部 Rust 库测试                     | 2904 通过，0 失败，6 个既有用例跳过             | 0          |
| TypeScript 类型检查                  | 通过                                            | 0          |
| 四语言文案修复前回归                 | 9 通过，4 个新增用例按预期失败                  | 1          |
| 前端全量首次运行                     | 1080 通过，1 个原有供应商流程用例触发 10 秒超时 | 1          |
| 供应商流程及语言覆盖原样复跑         | 20 通过，0 失败                                 | 0          |
| 前端全量原样复跑                     | 136 个测试文件、1081 个用例全部通过             | 0          |
| 修改后的四语言资源及前端测试格式检查 | Prettier 通过                                   | 0          |

前端首次全量运行与 Rust 编译并行。Rust 编译结束后，先原样复跑 `tests/integration/App.test.tsx` 和语言覆盖测试，再用相同参数重跑全部前端测试，均通过；没有修改断言、mock 或 10 秒超时阈值。

原始测试日志保留在本机：

- 修复前：`C:/Users/wuxianggujun/.fastctx/jobs/j-krr145/output.log`
- 修复后（包含签到回归及全部库测试）：`C:/Users/wuxianggujun/.fastctx/jobs/j-lj3yjn/output.log`
- 四语言修复前：`C:/Users/wuxianggujun/.fastctx/jobs/j-q78ryk/output.log`
- 前端全量首次运行：`C:/Users/wuxianggujun/.fastctx/jobs/j-cu8qdy/output.log`
- 前端原样复跑及格式检查：`C:/Users/wuxianggujun/.fastctx/jobs/j-wsf84v/output.log`

## 验证方法

所有 HTTP 回归均使用回环测试站点和虚构 Cookie，不执行真实账号签到。Rust 测试设置独立 `CC_SWITCH_TEST_HOME`；禁用增量编译并关闭当前包的测试调试符号，复用已有依赖构建，不清理用户缓存或构建产物。

在仓库根目录的 Git Bash 中运行：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
test_home=$(mktemp -d /tmp/cc-switch-cookie.XXXXXX)
export CC_SWITCH_TEST_HOME="$(cygpath -m "$test_home")"
export NO_PROXY=127.0.0.1,localhost,::1
export no_proxy="$NO_PROXY"
export CARGO_INCREMENTAL=0

# 签到模块回归
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked \
  --config 'profile.test.package.cc-switch.debug=0' services::checkin:: \
  -- --test-threads=1 --format=terse

# 全部 Rust 库测试
cargo test --manifest-path src-tauri/Cargo.toml --lib --locked \
  --config 'profile.test.package.cc-switch.debug=0' \
  -- --test-threads=1 --format=terse

pnpm typecheck
pnpm test:unit --maxWorkers=2 --minWorkers=1
```

源码、测试和四语言 JSON 均保持 UTF-8，并使用严格 UTF-8 解码和 JSON 解析校验；不使用系统 ANSI/GBK 编码转换。

## 使用与验收

1. 同站点两个条目均选择 browser 认证，在各自请求头中填写对应账号的 `Cookie`，不要依赖验证窗口内登录的账号。
2. 通常只需填写账号相关 Cookie，`cf_clearance` 由过闸补充。如果显式填写了 `cf_clearance`，该值也优先于自动凭证，过期时需移除或更新显式值。
3. 分别手动签到，通过站点返回的账号身份或账号余额确认 A、B 各自生效。未填写登录态时，不再回退使用 WebView 中的账号；结果仍由站点响应判定。
4. 本轮测试不等于真实 Cloudflare 站点验收，也不会更新正在运行的安装版。需要重新构建并使用新版本，源码修复才会进入桌面程序。

## 方案 B：仅记录，本轮不实现

- 每条签到条目分配独立、稳定的 `data_directory`，隔离 WebView2 profile 与 Cookie 存储。
- 增加“打开登录窗口”入口，允许用户在对应条目的独立 profile 内登录。

方案 A 没有新增上述目录隔离或登录入口，不能把本轮修复理解为已经实现浏览器多 profile 管理。
