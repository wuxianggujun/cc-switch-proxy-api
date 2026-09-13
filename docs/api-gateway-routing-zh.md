# API 网关选线架构

本文记录「API 接入」功能的当前实现状态、设计决策与未决问题。

**状态：已接通 Claude/OpenAI 三协议双向转发与请求日志。** 最新使用步骤、协议矩阵和验证说明见 `protocol-routing-request-logs-2026-09-09-zh.md`；早期轮询、签到和代理池检查见 `readiness-review-2026-09-08-zh.md`。

## 背景

原有的供应商切换靠改写 CLI 客户端的配置文件（`~/.claude/settings.json` 等）指向某个上游 + 某把 key。这个模型下一个供应商只能挂一把 key，且路由在「切换」那一刻就写死了。

目标是做本地 API 网关：同一上游类型下可配置多个接入，每个接入挂多把 key，请求按优先级选线，同优先级内轮询，额度用完自动换下一条。

## 两条链并存

| | 老链 | 新链 |
|---|---|---|
| 页面 | 供应商 | API 接入 |
| 表 | `providers` | `api_endpoints` + `api_keys` |
| 一个条目的 key 数 | 1 | N |
| 优先级字段 | 复用 `sort_index`（展示序兼任） | 独立 `priority` / `internal_priority` |
| 路由决定时机 | 切换那一刻写死 | 每个请求 |

**分叉判据**（`proxy/handler_context.rs` 的 `select_gateway_providers`）：查询本协议族启用的接入点，并允许 Claude/OpenAI 跨族接入点通过显式模型白名单加入。有则走新链，无则回落老链。配置存在但 key 不可用或模型不匹配，是网关不可用，不是未配置。

因此：

- API 接入页一条未配 → 全部请求走老链，行为与改造前一致
- 只在 Claude 下配了接入且白名单留空 → Claude 走新链，Codex / Gemini 仍走老链；显式声明模型后，Codex/Chat 可跨协议选中它
- 接入全部停用 → 未启用该网关 → 回落老链
- 接入启用但 key 全部不可用、模型不匹配 → 返回不可用，不回落旧账号
- 选线查询报错 → 显式返回数据库错误，不静默改变账号与计费来源

**注意**：同一上游一旦新链配了线路，老链就完全不参与，不是「新链失败再试老链」。混着走会让「当前供应商是谁」产生两个互相矛盾的答案。

老链是过渡期的安全网，不是终态。新链验证稳定后应清理掉——它与代理选线做的是同一件事。

## 选线算法

一句 SQL 把跨优先级候选铺平成有序序列（`database/dao/api_gateway.rs` 的 `select_route_candidates`）：

```sql
ORDER BY e.priority ASC,                  -- 层级，越小越优先
         k.internal_priority ASC,          -- 层内序
         k.last_used_at IS NOT NULL ASC,   -- 从未用过的排最前
         k.last_used_at ASC,               -- 其余按最久未用（LRU）
         k.id ASC                          -- 稳定兜底
```

三个关键设计：

**轮询顺序持久化。** `last_used_at` 使用单调毫秒标记，兼容旧秒值在首次使用后升级。请求的查询与首选 key 预占在同一 SQLite 连接锁内完成，不必等请求结束才轮换；预览查询不修改顺序。上游额度与冷却仍独立控制。

**不做「这一层全挂了吗」的判定。** 候选序列是 per-request 构建的，游标只活在这一次请求内。高优先级耗尽后自然落到低优先级；下一个请求重新从最高优先级开始排。因此降级纯临时、绝不粘滞，高优先级恢复后自动回切，不需要任何回切逻辑。

**优先级与展示序解耦。** `priority` 管路由，`sort_index` 管列表拖拽顺序。老链用 `sort_index` 兼任优先级，导致无法表达「同优先级」——这是新增独立字段的根因。

## 失败判罚

`database/dao/api_gateway_penalty.rs`。判罚粒度是**单把 key**，不是接入点：同一接入下 A key 被限流不影响 B key。

| 上游响应 | 处置 | 理由 |
|---|---|---|
| 400 / 404 / 405 / 413 / 422 / 501 | **不判罚** | 问题在调用方请求本身，换 key 无用，罚了只会污染健康度 |
| 401 | `hard_state = auth_invalid` | key 失效，轮询它只会持续失败 |
| 402 | `hard_state = quota_exhausted` | 余额不足，等充值或周期重置 |
| 403 + 封号词 | `hard_state = banned` | |
| 403 其余 | 冷却 300s | 可能是地域或临时策略限制 |
| 429 + 额度耗尽词 | `hard_state = quota_exhausted` | 等下个计费周期，继续轮询是浪费 |
| 429 其余 | 冷却 `retry-after` ?? 300s | 普通限流，几分钟就恢复 |
| 503 | 冷却 `retry-after` ?? 60s | |
| 529 | 冷却 30s | 上游过载，瞬时性强 |
| 其余 5xx | 冷却 60s | |
| 网络层失败 | 冷却 60s | 无状态码可依据 |

**429 的分岔是核心**：明确的 `insufficient_quota`、账单硬限额或余额不足可标记硬状态；通用 `RESOURCE_EXHAUSTED` / `quota exceeded` 也可能只是每分钟限流，不会据此永久禁用 key。`Retry-After` 支持秒数与 HTTP-date，并限制最大冷却。

冷却上限 1 小时，防止上游给出畸形的超大 `retry-after` 把 key 永久闲置。

**判罚独立于老链的 `ErrorCategory`。** 401 在老链是 Retryable（换一家可能有效），但对网关的这把 key 意味着它失效了，必须踢出候选集。二者语义不同，不能复用。

冷却到期自动回归候选集；`hard_state` 需人工清除（接入页的「恢复可用」按钮）或额度重置信号。

## 合成 Provider

所有 adapter 的凭据都从 `Provider.settings_config` JSON 读取。选线候选**合成为临时 Provider**（`proxy/gateway_route.rs`）以复用既有 adapter；转发层必须同时根据入口协议和上游协议处理请求、响应与 SSE，不能把“合成 Provider”误认为已经完成跨协议接线。

各上游的写入路径必须与 adapter 的读取路径对齐，否则 adapter 取不到凭据会报 ConfigError：

| 上游 | base_url 落点 | key 落点 |
|---|---|---|
| Claude | `env.ANTHROPIC_BASE_URL` | `env.ANTHROPIC_API_KEY` |
| OpenAI / Codex / DeepSeek | `base_url`（顶层）+ `env.OPENAI_BASE_URL` | `env.OPENAI_API_KEY` |
| Gemini | `env.GOOGLE_GEMINI_BASE_URL` | `env.GEMINI_API_KEY` |

同时冗余写入顶层 `base_url` 与 `api_key`，覆盖 adapter 的回退分支。

同时写入 `api_format`：OpenAI / DeepSeek 为 `openai_chat`，Codex 为 `openai_responses`，以便 Responses 客户端正确触发 Chat 转换。

OpenAI 族另标记 `auth_mode=bearer_only`，保证从 Claude 入口调用也使用 Bearer；Claude 接入标记 `api_key_field=ANTHROPIC_API_KEY`，保证从 Codex/Chat 入口调用也使用 `x-api-key`。

两个刻意的选择：

- **id 用 `gw:` + key_id**，不是 endpoint_id。轮询和判罚都发生在 key 粒度，熔断器、健康度、用量都该落在这一层。
- **不标 `category: "official"`**。该分类在老链有跳过连通检测、跳过 failover 的特权，会让网关线路绕过判罚。

## 哪些 app 不走网关

`gateway_upstreams_for` 映射 `Claude` / `Codex` / `Gemini` 的本族候选。Claude 与 Codex/Chat 还允许显式白名单命中的兼容跨族候选；同族先于跨族。Gemini 仍只使用原生协议族，不参与 Claude/OpenAI 的跨族选线。

OpenClaw、Hermes、Pi、GrokBuild、OpenCode 的专有接管命名空间暂不进入新链。这不代表这些客户端只能使用 OAuth：其中一些同样支持 API Key，也可以按协议手动指向通用网关入口。当前映射反映的是本轮接线范围，而不是这些客户端的完整能力。

多 OAuth 账号轮询是可以设计的，但不能直接套用 API Key 的 LRU：需要独立的账号刷新、会话粘性、`previous_response_id` 等有状态请求的归属处理，以及失败时的跨账号切换边界。当前仍保持固定账号；这是一项尚未实现的能力，而不是宣称 OAuth 天然不能轮询。

排除 `ClaudeDesktop` 的理由不同：它协议与 Claude 相同，但走独立的 `/claude-desktop/*` 命名空间与独立鉴权，并入会打错路由。

判据：**按请求入口与上游协议确定路由范围；通用网关做 key 轮询，专有账号接管仍走旧链。**

## 与故障转移的冲突

`proxy/failover_switch.rs` 的 `FailoverSwitchManager::try_switch` 在故障转移成功后会**真的改写数据库里的 current provider**、重建托盘菜单、发 `provider-switched` 事件。

在新模型里，轮询到第二条线路是**常态而非状态变更**。所有成功路径（含媒体、签名和 budget 重试）统一阻止旧供应商切换，并统一记录 key 成功；synthetic provider 不写旧 provider 健康表。

网关重试不依赖旧供应商的自动故障转移开关，但仍受 `max_retries + 1` 的单次请求尝试上限控制。

## 已知风险

按严重程度排列。

**1. 真实上游尚待用户配置验收。** 已使用回环 HTTP 服务验证 adapter 凭据、轮询、失败降级、模型过滤、Responses 转换与流式请求，但没有使用真实账号调用付费上游。

**2. OAuth 账号仍是固定绑定。** 新网关轮询的是 API Key，不会自动把网页登录账号变成可轮换池。

**3. 局域网认证未新增。** 默认本机监听；没有把通过本地回环测试解释成可以公开暴露的网关。

**4. token / cost 未落到 key 记账。** `record_api_key_success` 传 0，用量仍由 `proxy_request_logs` 统计。key 统计目前是请求/成败维度，不是完整的 key 级计费报表。

**5. 网关健康状态由 key 表管理。** 不再与旧 provider 熔断器重复判罚；计费用量仍以 `gw:{key_id}` 标识。新的请求日志页面同时展示接入点显示名、实际协议、模型、每次尝试和原始报文，独立于 key 级计费报表。

## 未决设计：轮询与固定指定

当前 `api_endpoints` 表**没有**策略字段，选线恒为轮询。

后续若增加策略，必须区分三种语义：`rotate` 轮换、`sticky` 优先复用且失败可以迁移、`pinned` 严格指定目标且失败不换。简单把 LRU 的 `ASC` 翻成 `DESC` 只能近似粘性，并不等于严格固定目标。

本轮没有为未确认的策略需求新增数据库字段或提升 schema。仅需固定一个 API Key 时，可以只启用这一把 key；有状态 OAuth 多账号策略需单独实现。

## 测试覆盖

| 文件 | 覆盖的不变量 |
|---|---|
| `database/dao/api_gateway.rs` | 层级优先、预占轮换、预览只读、并发判罚保留、作用域隔离、级联删除和 key 脱敏 |
| `database/dao/api_gateway_penalty.rs` | 客户端错误、401、明确余额耗尽、普通配额限制、Retry-After 秒数与 HTTP-date |
| `proxy/gateway_route.rs` | 凭据与协议落点、synthetic id、不获得 official 特权和显示名脱敏 |
| `proxy/gateway_tests.rs` | 实际回环 HTTP 请求的轮询、降级、模型发现/过滤、协议转换、流式/媒体重试及 HTTP/SOCKS 出口 |

不要仅根据失败文件名判断“与功能无关”。本轮进一步定位到 Windows 测试目录回退真实配置的问题，以及固定测试端口与运行中应用冲突的问题，并补上对应修复。请用隔离验证脚本运行，最终数量见本轮可用性检查报告。
