# 协议转发与请求日志（2026-09-09）

## 结论与使用入口

本轮接通的是**模型选线 + 双向协议转换 + 流式转发**，不是 HTTP 302 跳转，也不是只修改客户端供应商地址。

1. 在「API 接入」添加接入点，选择其**实际支持的上游协议**并添加 API Key。
2. OpenAI Chat / DeepSeek 选择 Chat 接入；Responses 选择 Codex 接入；Anthropic Messages 选择 Claude 接入。
3. 配置模型白名单，例如 `gpt-*`、`claude-*` 或具体模型名。跨协议选线必须显式命中白名单；空白名单只接受自身协议族入口。
4. 同族候选优先，随后才是兼容的跨族候选；族内按接入点优先级、Key 优先级及持久化 LRU 排序。要避免某个「不限模型」的同族接入先被尝试，应为它也配置准确的模型白名单。
5. 启动本地代理，将客户端地址指向页面显示的本机网关地址。不要把接入点配置成该网关自身的地址。
6. 打开侧边栏 **「请求日志」**。默认记录新请求；点击一行可查看「原始请求 / 上游尝试 / 最终响应 / 提示词对照」。

客户端使用的模型名不会因为入口是 Claude 或 Codex 就被猜测成另一种模型。协议由入口和选中接入点决定，模型按明确配置选线/映射。

## 协议矩阵

以下组合均有真实回环 HTTP 测试，包含普通 JSON、SSE，以及工具调用后携带工具结果的第二轮请求：

| 客户端入口 | Anthropic 上游 | Chat 上游 | Responses 上游 |
|---|---|---|---|
| `/v1/messages` | 透传 | 转换后还原 Messages | 转换后还原 Messages |
| `/v1/chat/completions` | 转换后还原 Chat | 透传 | 转换后还原 Chat |
| `/v1/responses` | 转换后还原 Responses | 转换后还原 Responses | 透传 |

保留现有 `/claude/v1/messages`、`/codex/v1/*`、Chat/Responses 无版本前缀等入口别名。接入点可以填写协议根地址，也可以填写对应的完整生成端点；不会重复追加路径。客户端查询参数继续传递。接入点地址本身仍禁止 userinfo、query 和 fragment，凭据放在 Key 字段。

`GET /v1/models` 和 `/models` 额外返回标准 OpenAI `object: list / data[]`，并保留 Codex 使用的 `models` 字段；只公告明确配置的模型名，不把 `claude-*` 之类路由规则当成真实模型。

### 本轮补齐的断链

- Chat 入口命中 Anthropic/Responses 后，不再直接返回错误的上游协议。
- 普通 JSON、SSE、上游忽略 `stream:true` 而返回 JSON 的情况走完整的反向转换。
- OpenAI 上游始终使用 Bearer；Anthropic API Key 使用 `x-api-key` 和 `anthropic-version`，不再由客户端入口误选认证头。
- Chat `response_format` 转为 Responses `text.format`；图片 URL/detail、旧版 functions、工具调用 ID、custom tools、推理回放字段做对应转换。
- Anthropic 目标保留 stop sequences、结构化输出、调用者 user ID，以及已有的思考/输出上限处理。
- `stream_options.include_usage` 控制 Chat 客户端的独立 `choices: []` 用量块，不关闭代理自身计量。
- SSE 中的失败不能伪装成 `finish_reason: stop`；传输截断也不会伪装成正常的 token 上限结束。
- Responses 流的首个有效输出等待使用统一截止时间，心跳不会无限延长等待；已经发送内容后不重新发起另一条上游请求。
- 非法请求 JSON 返回 400；Claude 入口错误使用 Anthropic 错误信封。

### 能力边界

- 没有对应目标协议语义的参数会显式报错，例如跨协议 `n > 1`、seed、非零 penalty、logprobs、音频输出，以及 Responses 目标不支持的 stop。需要这些 Chat 专用功能时使用 Chat 上游，不会静默删除其效果。
- Gemini 原生入口仍沿用原链，不进入这张 Claude/OpenAI 跨协议矩阵。
- 专有 OAuth 命名空间、多 OAuth 账号轮询、WebSocket/Realtime 和其它非生成 API 没有被本轮扩展成通用接口。
- 有状态 `previous_response_id` 仍受既有上游/历史恢复能力约束；本轮不宣称跨账号迁移上游会话。跨协议通用调用应携带完整消息/工具历史。
- 测试不依赖真实付费账号；具体上游可能有自己的模型、工具或字段限制，日志页面用于查看它实际返回的原因。

## 请求日志记录的是真实链路

```text
TCP 对端 → 原始 HTTP 请求 → 模型选线
        → 每次上游尝试：转换/整流后的真实报文、URL、出口代理、状态码、响应
        → 面向客户端的协议还原 → 最终响应 / 断开 / 错误
        → 本地 SQLite → Tauri commands → 请求日志页面
```

每条 API 请求记录有独立 `requestId`，响应头会返回 `x-ccswitch-request-id`。记录不依赖用量统计开关，也不需要上游提供 usage。内部 `/health`、`/status` 探针不记录，避免页面轮询淹没真实请求。

### 可查看的信息

- 真实 TCP 客户端 IP、端口、时间、HTTP 方法、路径、入口协议、请求模型。
- 原始请求头及 body；JSON 可切换原始文本与格式化视图。
- 每次实际 HTTP 尝试的供应商、Key 对应标识、目标 URL、协议、实际模型、出口代理、耗时及响应状态。
- 每次尝试的请求和响应 body，包含失败后换 Key、协议兼容重试等情况。
- 最终响应、首字节耗时、总耗时；区分 HTTP 错误、SSE 内错误、客户端断开、进程中断。
- 转换前后的 system/developer/user/assistant 消息及工具结果对照。
- 提示词/错误/请求 ID 搜索，IP、协议、状态码、时间、异常筛选，分页、实时刷新、复制脱敏 JSON。

「IP」不信任调用方自行填写的 `X-Forwarded-For`；该字段仍可在原始头中查看。这是应用层 HTTP 诊断记录，不是完整 TCP 抓包：底层帧边界、头大小写和压缩字节本身不作为可重放抓包保存。

### 保留与脱敏

默认：启用记录、保存报文、每个 body 最多 **1 MiB**、最多 **1000 条 / 3 天 / 128 MiB**。在「记录设置」调整，或只保留元数据。

- 单 body 可以配置 4 KiB–4 MiB。单次请求的多个尝试共享有界捕获预算，最多 16 MiB；不会无限累积流或重试报文。
- 超限会明确显示已捕获/观察到的字节数。**截断的是日志，不是实际转发请求。** 对很长的提示词，提高上限后再复现。
- 二进制或无法安全解码的内容不当成乱码保存；记录大小及原因。可解码的压缩 body 以 UTF-8 文本查看。
- Authorization、API Key、Cookie、URL 凭据及已知认证值脱敏；结构化密码/密钥字段同样脱敏。提示词本身保留用于排查。
- SQLite 表 `request_traces` 与计费用的 `proxy_request_logs` 分开，不改写用量统计。
- 新请求、更新、设置变更、读取列表及启动恢复时执行保留上限检查。
- 清空后，尚在结束的旧请求不会将记录重新插回；异步更新有 revision 防止旧状态覆盖新状态。
- 崩溃遗留的进行中记录在下次启动标记为 `interrupted`。
- 请求日志不参与 WebDAV/S3 配置同步；手工完整数据库备份仍属于完整备份。

排查某次“提示词被拦截”时，先筛选异常并找到请求，比较原始请求与实际上游请求，再查看对应上游返回的原始 error/code/param。HTTP 400 的无效字段、认证失败、额度不足、content_filter 和本地转换失败不应混为一类。

## 代码结构

| 模块 | 职责 |
|---|---|
| `database/dao/api_gateway.rs` | 模型白名单、候选预占、轮询、跨族范围 |
| `proxy/gateway_route.rs` | 将候选合成为携带正确协议/认证信息的 Provider |
| `proxy/forwarder.rs` | 请求转换、目标端点、发送、错误/重试边界 |
| `proxy/chat_bridge.rs` | Chat 入口的上游响应/SSE 反向桥接 |
| `proxy/providers/*chat_entry.rs` | Chat↔Responses 表示和流式状态机 |
| `proxy/request_trace.rs` | 请求与流的旁路捕获、脱敏、生命周期 |
| `database/dao/request_traces.rs` | Schema 21 日志表、查询、配置、保留和恢复 |
| `commands/request_traces.rs` | 异步 Tauri IPC，阻塞数据库操作不占用事件线程 |
| `src/components/requestLogs/` | 列表、设置、详情、提示词对照 |
| `src/lib/api/requestTraces.ts` / `src/lib/query/requestTraces.ts` | 类型化 API 与缓存/刷新 |

所有新增源码、四语言文案与记录文本均为 UTF-8；Rust `String`/serde_json 和前端 UTF-8 页面一致，SSE 分片按完整 UTF-8 字符解析。

## 运行与验证

在仓库根目录运行：

```powershell
pnpm dev
```

构建含前端资源的独立调试版（不生成安装器）：

```powershell
pnpm tauri build --debug --no-bundle
```

隔离验证，不接触真实 CLI 配置；脚本结束后恢复环境变量，保留独立测试目录供排查：

```powershell
pwsh -NoProfile -File .\scripts\verify-core.ps1 -Full
```

### 本轮最终验证

| 验证 | 结果 | 退出码 |
|---|---|---|
| Rust 全量库测试 | 2988 通过，7 个原有忽略用例，0 失败 | 0 |
| 前端全量 | 142 文件、1109 用例通过 | 0 |
| 最终 UI 改动专项及四语言覆盖 | 30 用例通过 | 0 |
| TypeScript 类型检查 | 通过 | 0 |
| 浏览器实际交互 | 筛选、报文详情、提示词对照、复制、设置、清空、800 px 布局，页面异常 0 | 0 |
| Tauri 独立 Windows Debug 构建 | 前端资源一并构建，不依赖 Vite 开发服务器；未生成安装器 | 0 |

调试版产物：`src-tauri/target/debug/cc-switch-proxy.exe`。未自动停止、替换或启动现有安装版；退出旧版后可手动运行该产物。未提交或推送代码。

浏览器检查运行真实 React 页面，IPC 使用专门的测试数据；Rust 回环测试运行真实 TCP、路由、转换器与 SQLite。这两类验证不冒充真实上游账号验收。截图及临时浏览器夹具保留在 `.tmp/request-logs-qa/`，已排除 Git 跟踪。

参考：[CLIProxyAPI 本地 translator 源码](../../CLIProxyAPI/internal/translator/)、[OpenAI Docs：Responses 迁移](https://developers.openai.com/api/docs/guides/migrate-to-responses)、[OpenAI Docs：流式响应](https://developers.openai.com/api/docs/guides/streaming-responses)。
