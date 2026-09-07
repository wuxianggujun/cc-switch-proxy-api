# API 网关选线架构

本文记录「API 接入」功能的当前实现状态、设计决策与未决问题。

**状态：已实现，未经真机测试。** 下方「已知风险」一节列出了尚未验证的部分。

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

**分叉判据**（`proxy/handler_context.rs` 的 `select_gateway_providers`）：查询该上游类型下有无启用的接入线路。有则走新链，无则回落老链。

因此：

- API 接入页一条未配 → 全部请求走老链，行为与改造前一致
- 只在 Claude 下配了接入 → Claude 走新链，Codex / Gemini 仍走老链
- 接入全部停用 → 选线返回空 → 自动回落老链
- 选线查询报错 → 返回空回落老链（网关是增量能力，其故障不应拖垮老链）

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

**轮询不用内存计数器。** 靠 `api_keys.last_used_at` 做 LRU 排序，状态就在表里，重启不丢、无需锁。并发下两个请求可能拿到同一把 key，这是可接受的——真正的限流由上游和冷却机制兜底。这一点照搬 Aether。

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

**429 的分岔是核心**：额度耗尽与普通限流都表现为 429，但处置完全不同。靠 body 关键词区分（`insufficient_quota`、`resource_exhausted`、`quota exceeded` 等）。

冷却上限 1 小时，防止上游给出畸形的超大 `retry-after` 把 key 永久闲置。

**判罚独立于老链的 `ErrorCategory`。** 401 在老链是 Retryable（换一家可能有效），但对网关的这把 key 意味着它失效了，必须踢出候选集。二者语义不同，不能复用。

冷却到期自动回归候选集；`hard_state` 需人工清除（接入页的「恢复可用」按钮）或额度重置信号。

## 合成 Provider

所有 adapter 的凭据都从 `Provider.settings_config` JSON 读取。因此接线最省的做法是把选线候选**合成为临时 Provider**（`proxy/gateway_route.rs`），整条转发链、全部协议转换器零改动复用。

各上游的写入路径必须与 adapter 的读取路径对齐，否则 adapter 取不到凭据会报 ConfigError：

| 上游 | base_url 落点 | key 落点 |
|---|---|---|
| Claude | `env.ANTHROPIC_BASE_URL` | `env.ANTHROPIC_AUTH_TOKEN` |
| OpenAI / Codex / DeepSeek | `base_url`（顶层）+ `env.OPENAI_BASE_URL` | `env.OPENAI_API_KEY` |
| Gemini | `env.GOOGLE_GEMINI_BASE_URL` | `env.GEMINI_API_KEY` |

同时冗余写入顶层 `base_url` 与 `api_key`，覆盖 adapter 的回退分支。

两个刻意的选择：

- **id 用 `gw:` + key_id**，不是 endpoint_id。轮询和判罚都发生在 key 粒度，熔断器、健康度、用量都该落在这一层。
- **不标 `category: "official"`**。该分类在老链有跳过连通检测、跳过 failover 的特权，会让网关线路绕过判罚。

## 哪些 app 不走网关

`gateway_upstream_for` 只映射 `Claude` / `Codex` / `Gemini`。

排除 OpenClaw、Hermes、Pi、GrokBuild、OpenCode：这些是 **OAuth / 托管账号**，不是 key。轮询的前提是「候选彼此等价，随便挑一个都能完成请求」，而账号绑定着自己的会话和配额，轮询到另一个账号意味着对话中途换人——上下文断裂、配额算错账。原代码对此有明确注释（`proxy/provider_router.rs` 的 `provider_supports_failover`）：复用入站 token 打到另一张账号卡上会跨越账号边界。

这不是「暂未实现」，是语义上就不该轮询。它们要的是「固定用这个账号，坏了报错让我重新登录」。

排除 `ClaudeDesktop` 的理由不同：它协议与 Claude 相同，但走独立的 `/claude-desktop/*` 命名空间与独立鉴权，并入会打错路由。

判据一句话：**能填 API key 的走网关轮询，靠账号登录的不走。**

## 与故障转移的冲突

`proxy/failover_switch.rs` 的 `FailoverSwitchManager::try_switch` 在故障转移成功后会**真的改写数据库里的 current provider**、重建托盘菜单、发 `provider-switched` 事件。

在新模型里，轮询到第二条线路是**常态而非状态变更**。照原样会导致 current 被反复改写、UI 抖动。已在 `forwarder.rs` 的主成功路径用 `is_gateway_provider` 短路。

## 已知风险

按严重程度排列。

**1. 尚未真机验证。** 全部结论来自单元测试与代码静态分析。以下路径**从未跑过真实请求**：合成 Provider 能否被各 adapter 正确消费、选线分叉在真实流量下是否按预期回落、判罚写库是否与流式响应的生命周期冲突。

**2. 媒体重试路径的 `try_switch` 未短路。** `forwarder.rs` 有三处额外的成功路径（图片降级重试、整流器重试等），其中的 `try_switch` 调用尚未加 `is_gateway_provider` 判断。触发条件是网关线路遇到图片输入被拒后重试成功——此时可能仍会改写 current provider。

**3. `retry-after` 精度丢失。** 该响应头没有保留在 `ProxyError` 里，判罚时拿不到，429 因此走默认 300 秒冷却而非上游给的精确值。可接受的降级，但上游若要求更短的等待，会造成不必要的闲置。

**4. token / cost 未落到 key 记账。** `record_api_key_success` 传 0，用量统计由 `proxy_request_logs` 那条链负责。因此接入页上的 key 统计只有请求数与成败数，没有 token 与成本。这是为避免双计的刻意选择。

**5. 熔断器仍是 provider 粒度。** 熔断器 key 是 `app_type:provider_id`，网关链传入的是 `gw:{key_id}`，所以实际上已经是 key 粒度——但 `provider_health` 表的主键、`proxy_request_logs.provider_id` 也会写入这个合成 id，与老链的真实 provider id 混在同一列。查询与统计时需要靠 `gw:` 前缀区分，目前没有任何地方做这个区分。

## 未决设计：轮询与固定指定

当前 `api_endpoints` 表**没有**策略字段，选线恒为轮询。

需求是同时支持「多账号轮询」与「固定指定单个账号」。这两者不是对立功能——固定指定就是候选序列长度为 1 的轮询。Aether 的做法是共用同一句查询，只把 `last_used_at ASC` 翻成 `DESC`，从「挑最久没用的」变成「黏住刚用过的」。

实现路径（未动工）：

1. `api_endpoints` 加 `strategy TEXT NOT NULL DEFAULT 'rotate'`，取值 `rotate` / `pinned`
2. 迁移到 `SCHEMA_VERSION = 21`
3. 选线 SQL 按策略切排序方向
4. 接入页加模式开关

这样 Claude 可以走轮询、Codex 固定某个号，互不干扰，粒度比「在设置里配一个全局的单供应商」更细。

## 测试覆盖

| 文件 | 测试数 | 覆盖的不变量 |
|---|---|---|
| `database/dao/api_gateway.rs` | 10 | 层级压过 LRU、同层按最久未用轮询且记账后指针前移、冷却自动回归而硬状态不会、停用项排除、上游与模型作用域隔离、级联删除、明文 key 不出现在序列化输出 |
| `database/dao/api_gateway_penalty.rs` | 9 | 客户端错误不判罚、401 标记失效、`retry-after` 采纳与上限、额度耗尽优先于限流、封号与临时限制区分 |
| `proxy/gateway_route.rs` | 6 | 三个上游的凭据落点、合成 Provider 可识别且携带 key_id、不获得 official 特权、显示名脱敏 |

`cargo test --lib` 全量 2854 passed / 6 failed。那 6 个失败在 `model_pricing`、`provider`、`skill`，与本功能零关联（这些文件完全不提 `api_gateway`），单独运行也失败，属既有的测试环境依赖问题。
