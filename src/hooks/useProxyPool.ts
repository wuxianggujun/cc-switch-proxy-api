/**
 * 代理池 React Hooks
 *
 * 订阅 / 节点 / 统计 / 配置 / 租约五组数据，写操作后按需失效。
 * 节点变动会影响统计，所以刷新与探测都要连带失效 stats。
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  addSubscription,
  clearAllLeases,
  clearLease,
  deleteSubscription,
  getConfig,
  getStats,
  listLeases,
  listNodes,
  listSubscriptions,
  probeNodes,
  refreshSubscription,
  resetCircuit,
  setConfig,
  updateSubscription,
  type AddSubscriptionPayload,
  type PoolConfig,
  type RefreshOutcome,
  type Subscription,
} from "@/lib/api/proxyPool";
import { extractErrorMessage } from "@/utils/errorUtils";
import { translateProxyPoolError } from "@/utils/proxyPoolErrors";

export const proxyPoolKeys = {
  all: ["proxyPool"] as const,
  subscriptions: ["proxyPool", "subscriptions"] as const,
  nodes: ["proxyPool", "nodes"] as const,
  stats: ["proxyPool", "stats"] as const,
  config: ["proxyPool", "config"] as const,
  leases: ["proxyPool", "leases"] as const,
};

// ---- 查询 ----

export function useProxyPoolSubscriptions() {
  return useQuery({
    queryKey: proxyPoolKeys.subscriptions,
    queryFn: listSubscriptions,
  });
}

export function useProxyPoolNodes() {
  return useQuery({
    queryKey: proxyPoolKeys.nodes,
    queryFn: listNodes,
  });
}

export function useProxyPoolStats() {
  return useQuery({
    queryKey: proxyPoolKeys.stats,
    queryFn: getStats,
  });
}

export function useProxyPoolConfig() {
  return useQuery({
    queryKey: proxyPoolKeys.config,
    queryFn: getConfig,
  });
}

export function useProxyPoolLeases() {
  return useQuery({
    queryKey: proxyPoolKeys.leases,
    queryFn: listLeases,
  });
}

// ---- 写操作 ----

/**
 * 把刷新结果转成提示。
 *
 * 被跳过的协议必须显式说出来，否则用户会以为订阅解析坏了 ——
 * 实际是 vmess/vless/trojan 这类协议后端不支持。
 */
function notifyRefreshOutcome(
  outcome: RefreshOutcome,
  t: (key: string, options?: Record<string, unknown>) => string,
): void {
  toast.success(t("proxyPool.toast.refreshed", { count: outcome.nodeCount }));
  if (outcome.skippedUnsupported > 0) {
    toast.warning(
      t("proxyPool.toast.skipped", {
        count: outcome.skippedUnsupported,
        protocols: outcome.skippedProtocols.join(", "),
      }),
    );
  }
}

export function useAddProxyPoolSubscription() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (payload: AddSubscriptionPayload) => addSubscription(payload),
    onSuccess: (outcome) => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.all });
      notifyRefreshOutcome(outcome, t);
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.addFailed"),
      );
    },
  });
}

/**
 * 更新订阅（启用开关、间隔等）。
 *
 * 后端整条 upsert 且 content 不下发，inline 订阅的正文会在此丢失，
 * 因此 UI 只对 remote 订阅暴露该操作。
 */
export function useUpdateProxyPoolSubscription() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (subscription: Subscription) =>
      updateSubscription(subscription),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.all });
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.saveFailed"),
      );
    },
  });
}

export function useDeleteProxyPoolSubscription() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (id: string) => deleteSubscription(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.all });
      toast.success(t("proxyPool.toast.deleted"));
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.deleteFailed"),
      );
    },
  });
}

export function useRefreshProxyPoolSubscription() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (id: string) => refreshSubscription(id),
    onSuccess: (outcome) => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.all });
      notifyRefreshOutcome(outcome, t);
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.refreshFailed"),
      );
    },
  });
}

export function useSetProxyPoolConfig() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (config: PoolConfig) => setConfig(config),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.all });
      toast.success(t("proxyPool.toast.configSaved"));
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.saveFailed"),
      );
    },
  });
}

/** 探测节点。传空数组表示全量探测。 */
export function useProbeProxyPoolNodes() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (nodeHashes: string[]) => probeNodes(nodeHashes),
    onSuccess: (probed) => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.nodes });
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.stats });
      toast.success(t("proxyPool.toast.probed", { count: probed }));
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.probeFailed"),
      );
    },
  });
}

export function useResetProxyPoolCircuit() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (nodeHash: string) => resetCircuit(nodeHash),
    onSuccess: (reset) => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.nodes });
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.stats });
      // false 表示该节点本来就没熔断，不该报成功
      if (reset) {
        toast.success(t("proxyPool.toast.circuitReset"));
      } else {
        toast.info(t("proxyPool.toast.circuitNotOpen"));
      }
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.resetFailed"),
      );
    },
  });
}

export function useClearProxyPoolLease() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (stickyKey: string) => clearLease(stickyKey),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.leases });
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.stats });
      toast.success(t("proxyPool.toast.leaseCleared"));
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.clearLeaseFailed"),
      );
    },
  });
}

export function useClearAllProxyPoolLeases() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: () => clearAllLeases(),
    onSuccess: (count) => {
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.leases });
      void queryClient.invalidateQueries({ queryKey: proxyPoolKeys.stats });
      toast.success(t("proxyPool.toast.allLeasesCleared", { count }));
    },
    onError: (error) => {
      toast.error(
        translateProxyPoolError(extractErrorMessage(error), t) ||
          t("proxyPool.toast.clearLeaseFailed"),
      );
    },
  });
}
