/**
 * 代理池 API
 *
 * 聚合订阅里的 HTTP/SOCKS5 节点，做健康探测、熔断与粘性路由。
 * 后端结构体统一 camelCase 序列化，字段名与此处一致。
 */

import { invoke } from "@tauri-apps/api/core";

/** 出站协议。后端只实现 reqwest 原生支持的三种。 */
export type NodeProtocol = "http" | "https" | "socks5";

/** 订阅来源：remote 需要 URL，inline 需要粘贴正文。 */
export type SubscriptionSource = "remote" | "inline";

/**
 * 订阅记录。
 *
 * 注意 `content` 不在其中：后端标了 skip_serializing，正文只写不读。
 */
export interface Subscription {
  id: string;
  name: string;
  source: SubscriptionSource;
  /** remote 时为订阅地址；inline 时为空 */
  url: string;
  enabled: boolean;
  /** 自动刷新间隔（秒），0 表示不自动刷新 */
  updateIntervalSecs: number;
  createdAtMs: number;
  updatedAtMs: number;
  /** 上次刷新解析出的节点数 */
  nodeCount: number;
  /** 上次刷新的错误，存在即代表该行处于错误态 */
  lastError?: string;
}

/** 新增订阅的入参。content 仅 inline 需要。 */
export interface AddSubscriptionPayload {
  name: string;
  source: SubscriptionSource;
  url: string;
  content: string;
  updateIntervalSecs: number;
}

/** 刷新结果。skippedProtocols 用于提示"这些节点当前不支持"。 */
export interface RefreshOutcome {
  subscriptionId: string;
  nodeCount: number;
  skippedUnsupported: number;
  skippedProtocols: string[];
}

/** 节点健康状态 */
export interface NodeHealth {
  /** 连续失败次数，成功即归零 */
  failureCount: number;
  /** 熔断起始时间（ms）。0 表示未熔断。 */
  circuitOpenSinceMs: number;
  /** 探测到的出口 IP，粘性路由靠它做同 IP 迁移 */
  egressIp?: string;
  /** 延迟 EWMA（ms）。未探测出有效值时为 undefined。 */
  latencyEwmaMs?: number;
  lastProbeAtMs: number;
}

/**
 * 节点视图。后端对 ProxyNode 用了 serde flatten，
 * 所以 hash/protocol/host/port/tag 都在顶层，只有 health 是嵌套的。
 * password 永远不下发。
 */
export interface NodeView {
  hash: string;
  protocol: NodeProtocol;
  host: string;
  port: number;
  username?: string;
  tag: string;
  health: NodeHealth;
  /** 所属订阅（跨订阅去重后可能多个） */
  subscriptionIds: string[];
}

/** 池的聚合统计 */
export interface PoolStats {
  totalNodes: number;
  healthyNodes: number;
  circuitOpenNodes: number;
  uniqueEgressIps: number;
  activeLeases: number;
  subscriptionCount: number;
}

/** 池的调度参数 */
export interface PoolConfig {
  enabled: boolean;
  maxConsecutiveFailures: number;
  circuitCooldownSecs: number;
  leaseTtlSecs: number;
  /** 0 表示关闭主动探测 */
  probeIntervalSecs: number;
  probeTimeoutSecs: number;
  probeConcurrency: number;
  egressProbeUrl: string;
  latencyProbeUrl: string;
}

/** 粘性租约：业务身份 → 具体节点 */
export interface Lease {
  stickyKey: string;
  nodeHash: string;
  egressIp?: string;
  createdAtMs: number;
  lastUsedAtMs: number;
}

// ---- 订阅 ----

export async function listSubscriptions(): Promise<Subscription[]> {
  return invoke<Subscription[]>("pp_list_subscriptions");
}

/** 新增订阅并立即刷新一次 */
export async function addSubscription(
  payload: AddSubscriptionPayload,
): Promise<RefreshOutcome> {
  return invoke<RefreshOutcome>("pp_add_subscription", { ...payload });
}

/**
 * 更新订阅。
 *
 * 后端按整条记录 upsert，而 content 不会下发到前端，
 * 所以这里回传的记录会把 inline 正文覆盖为空 —— 调用方需知悉该限制。
 */
export async function updateSubscription(
  subscription: Subscription,
): Promise<void> {
  return invoke("pp_update_subscription", { subscription });
}

export async function deleteSubscription(id: string): Promise<void> {
  return invoke("pp_delete_subscription", { id });
}

export async function refreshSubscription(id: string): Promise<RefreshOutcome> {
  return invoke<RefreshOutcome>("pp_refresh_subscription", { id });
}

// ---- 节点 ----

export async function listNodes(): Promise<NodeView[]> {
  return invoke<NodeView[]>("pp_list_nodes");
}

/**
 * 探测节点，返回被探测的节点数。
 *
 * @param nodeHashes 空数组表示探测全部
 */
export async function probeNodes(nodeHashes: string[]): Promise<number> {
  return invoke<number>("pp_probe_nodes", { nodeHashes });
}

/** 手动解除熔断。返回 false 表示该节点本来就没熔断。 */
export async function resetCircuit(nodeHash: string): Promise<boolean> {
  return invoke<boolean>("pp_reset_circuit", { nodeHash });
}

// ---- 统计与配置 ----

export async function getStats(): Promise<PoolStats> {
  return invoke<PoolStats>("pp_get_stats");
}

export async function getConfig(): Promise<PoolConfig> {
  return invoke<PoolConfig>("pp_get_config");
}

export async function setConfig(config: PoolConfig): Promise<void> {
  return invoke("pp_set_config", { config });
}

// ---- 粘性租约 ----

export async function listLeases(): Promise<Lease[]> {
  return invoke<Lease[]>("pp_list_leases");
}

/** 解绑单条租约。返回 false 表示该 key 没有租约。 */
export async function clearLease(stickyKey: string): Promise<boolean> {
  return invoke<boolean>("pp_clear_lease", { stickyKey });
}

/** 清空所有租约，返回清除条数 */
export async function clearAllLeases(): Promise<number> {
  return invoke<number>("pp_clear_all_leases");
}
