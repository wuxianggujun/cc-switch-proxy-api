/**
 * 代理池主页面
 *
 * 自上而下：统计条 + 总开关 → 订阅 → 节点 → 高级配置 → 粘性租约。
 * 租约是诊断信息，默认折叠。
 */

import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  Loader2,
  Plus,
  RefreshCw,
  Trash2,
  Zap,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ProxyPoolNodesTable } from "./ProxyPoolNodesTable";
import { ProxyPoolSubscriptionModal } from "./ProxyPoolSubscriptionModal";
import {
  useAddProxyPoolSubscription,
  useClearAllProxyPoolLeases,
  useClearProxyPoolLease,
  useDeleteProxyPoolSubscription,
  useProbeProxyPoolNodes,
  useProxyPoolConfig,
  useProxyPoolLeases,
  useProxyPoolNodes,
  useProxyPoolStats,
  useProxyPoolSubscriptions,
  useRefreshProxyPoolSubscription,
  useResetProxyPoolCircuit,
  useSetProxyPoolConfig,
  useUpdateProxyPoolSubscription,
} from "@/hooks/useProxyPool";
import type { PoolConfig, Subscription } from "@/lib/api/proxyPool";

function formatTimestamp(ms: number): string {
  if (!ms) return "-";
  return new Date(ms).toLocaleString();
}

/** 数字型配置字段，统一渲染避免十个近似的 input 块 */
const NUMERIC_FIELDS = [
  { key: "maxConsecutiveFailures", min: 1 },
  { key: "circuitCooldownSecs", min: 0 },
  { key: "leaseTtlSecs", min: 0 },
  { key: "probeIntervalSecs", min: 0 },
  { key: "probeTimeoutSecs", min: 1 },
  { key: "probeConcurrency", min: 1 },
] as const satisfies ReadonlyArray<{
  key: keyof PoolConfig;
  min: number;
}>;

export function ProxyPoolPanel() {
  const { t } = useTranslation();

  const { data: stats } = useProxyPoolStats();
  const { data: subscriptions = [], isLoading: subsLoading } =
    useProxyPoolSubscriptions();
  const { data: nodes = [] } = useProxyPoolNodes();
  const { data: config } = useProxyPoolConfig();
  const { data: leases = [] } = useProxyPoolLeases();

  const addSub = useAddProxyPoolSubscription();
  const updateSub = useUpdateProxyPoolSubscription();
  const deleteSub = useDeleteProxyPoolSubscription();
  const refreshSub = useRefreshProxyPoolSubscription();
  const setPoolConfig = useSetProxyPoolConfig();
  const probeNodes = useProbeProxyPoolNodes();
  const resetCircuit = useResetProxyPoolCircuit();
  const clearLease = useClearProxyPoolLease();
  const clearAllLeases = useClearAllProxyPoolLeases();

  const [formOpen, setFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Subscription | null>(null);
  const [clearAllOpen, setClearAllOpen] = useState(false);
  const [probingHashes, setProbingHashes] = useState<Set<string>>(new Set());

  // 高级配置用本地草稿 + dirty，跟 GlobalProxySettings 一致
  const [draft, setDraft] = useState<PoolConfig | null>(null);
  const [dirty, setDirty] = useState(false);

  // 总开关也会失效 config 查询，若无条件同步会把用户在高级配置里
  // 未保存的编辑冲掉，所以 dirty 期间不接受远端值。
  useEffect(() => {
    if (config && !dirty) {
      setDraft(config);
    }
  }, [config, dirty]);

  const statCards = useMemo(
    () => [
      { key: "totalNodes", value: stats?.totalNodes ?? 0 },
      { key: "healthyNodes", value: stats?.healthyNodes ?? 0 },
      { key: "circuitOpenNodes", value: stats?.circuitOpenNodes ?? 0 },
      { key: "uniqueEgressIps", value: stats?.uniqueEgressIps ?? 0 },
      { key: "activeLeases", value: stats?.activeLeases ?? 0 },
    ],
    [stats],
  );

  const nodeByHash = useMemo(
    () => new Map(nodes.map((node) => [node.hash, node])),
    [nodes],
  );

  /** 总开关只改 enabled，其余字段保持远端值 */
  const handleToggleEnabled = (enabled: boolean) => {
    if (!config) return;
    setPoolConfig.mutate({ ...config, enabled });
  };

  const handleProbeOne = async (hash: string) => {
    setProbingHashes((prev) => new Set(prev).add(hash));
    try {
      await probeNodes.mutateAsync([hash]);
    } finally {
      setProbingHashes((prev) => {
        const next = new Set(prev);
        next.delete(hash);
        return next;
      });
    }
  };

  /**
   * 保存高级配置。
   *
   * enabled 由总开关单独管，草稿里的那份可能已过期，
   * 所以回写时以远端值为准，避免把开关状态改回去。
   */
  const handleSaveConfig = () => {
    if (!draft) return;
    setPoolConfig.mutate(
      { ...draft, enabled: config?.enabled ?? draft.enabled },
      { onSuccess: () => setDirty(false) },
    );
  };

  const updateDraft = (patch: Partial<PoolConfig>) => {
    setDraft((prev) => (prev ? { ...prev, ...patch } : prev));
    setDirty(true);
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-6 pb-8">
      {/* 统计条 + 总开关 */}
      <section className="rounded-lg border border-border bg-muted/20 p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Switch
              id="pp-enabled"
              checked={config?.enabled ?? false}
              disabled={!config || setPoolConfig.isPending}
              onCheckedChange={handleToggleEnabled}
              aria-label={t("proxyPool.enableLabel")}
            />
            <Label htmlFor="pp-enabled" className="cursor-pointer text-sm">
              {t("proxyPool.enableLabel")}
            </Label>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => probeNodes.mutate([])}
              disabled={probeNodes.isPending || nodes.length === 0}
            >
              {probeNodes.isPending ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : (
                <Zap className="mr-1 h-3.5 w-3.5" />
              )}
              {t("proxyPool.probeAll")}
            </Button>
            <Button
              size="sm"
              onClick={() => setFormOpen(true)}
              disabled={addSub.isPending}
            >
              <Plus className="mr-1 h-4 w-4" />
              {t("proxyPool.addSubscription")}
            </Button>
          </div>
        </div>

        <dl className="mt-4 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
          {statCards.map(({ key, value }) => (
            <div
              key={key}
              className="rounded-md border border-border bg-background px-3 py-2"
            >
              <dt className="text-xs text-muted-foreground">
                {t(`proxyPool.stats.${key}`)}
              </dt>
              <dd className="mt-0.5 text-lg font-semibold tabular-nums">
                {value}
              </dd>
            </div>
          ))}
        </dl>

        {!config?.enabled && (
          <p className="mt-3 text-xs text-muted-foreground">
            {t("proxyPool.disabledHint")}
          </p>
        )}
      </section>

      {/* 订阅 */}
      <section className="space-y-2">
        <h2 className="text-sm font-semibold">
          {t("proxyPool.subscriptions.title")}
        </h2>
        {subsLoading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : subscriptions.length === 0 ? (
          <p className="rounded-lg border border-dashed border-border px-3 py-8 text-center text-sm text-muted-foreground">
            {t("proxyPool.subscriptions.empty")}
          </p>
        ) : (
          <ul className="space-y-2">
            {subscriptions.map((sub) => (
              <li
                key={sub.id}
                className="rounded-lg border border-border px-3 py-2.5"
              >
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
                  <span className="min-w-0 flex-1 truncate text-sm font-medium">
                    {sub.name}
                  </span>
                  <Badge variant="outline" className="text-[10px]">
                    {t(`proxyPool.source.${sub.source}`)}
                  </Badge>
                  <span className="text-xs text-muted-foreground">
                    {t("proxyPool.subscriptions.nodeCount", {
                      count: sub.nodeCount,
                    })}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {t("proxyPool.subscriptions.updatedAt", {
                      time: formatTimestamp(sub.updatedAtMs),
                    })}
                  </span>

                  <div className="ml-auto flex items-center gap-1.5">
                    <Switch
                      checked={sub.enabled}
                      // inline 订阅的正文不下发，回传会把它清空，故禁用开关
                      disabled={sub.source === "inline" || updateSub.isPending}
                      onCheckedChange={(enabled) =>
                        updateSub.mutate({ ...sub, enabled })
                      }
                      aria-label={t("proxyPool.subscriptions.toggleFor", {
                        name: sub.name,
                      })}
                      title={
                        sub.source === "inline"
                          ? t("proxyPool.subscriptions.inlineToggleBlocked")
                          : undefined
                      }
                    />
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      disabled={refreshSub.isPending}
                      onClick={() => refreshSub.mutate(sub.id)}
                      aria-label={t("proxyPool.subscriptions.refreshFor", {
                        name: sub.name,
                      })}
                      title={t("common.refresh")}
                    >
                      <RefreshCw className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7 text-destructive hover:text-destructive"
                      onClick={() => setDeleteTarget(sub)}
                      aria-label={t("proxyPool.subscriptions.deleteFor", {
                        name: sub.name,
                      })}
                      title={t("common.delete")}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>

                {sub.source === "remote" && sub.url && (
                  <p className="mt-1 truncate font-mono text-xs text-muted-foreground">
                    {sub.url}
                  </p>
                )}

                {sub.lastError && (
                  <p
                    className="mt-1.5 flex items-start gap-1.5 text-xs text-destructive"
                    role="alert"
                  >
                    <AlertTriangle
                      className="mt-0.5 h-3.5 w-3.5 shrink-0"
                      aria-hidden="true"
                    />
                    <span className="min-w-0 break-all">{sub.lastError}</span>
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      {/* 节点 */}
      <section className="space-y-2">
        <h2 className="text-sm font-semibold">{t("proxyPool.nodes.title")}</h2>
        <ProxyPoolNodesTable
          nodes={nodes}
          probingHashes={probingHashes}
          onProbeNode={handleProbeOne}
          onResetCircuit={(hash) => resetCircuit.mutate(hash)}
        />
      </section>

      {/* 高级配置 + 租约 */}
      <Accordion type="multiple" className="w-full">
        <AccordionItem value="advanced">
          <AccordionTrigger className="text-sm font-semibold">
            {t("proxyPool.advanced.title")}
          </AccordionTrigger>
          <AccordionContent>
            {draft ? (
              <div className="space-y-4 pt-1">
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
                  {NUMERIC_FIELDS.map(({ key, min }) => (
                    <div key={key} className="space-y-1.5">
                      <Label htmlFor={`pp-${key}`} className="text-xs">
                        {t(`proxyPool.advanced.${key}`)}
                      </Label>
                      <Input
                        id={`pp-${key}`}
                        type="number"
                        min={min}
                        value={String(draft[key])}
                        onChange={(e) => {
                          const parsed = Number(e.target.value);
                          updateDraft({
                            [key]: Number.isFinite(parsed)
                              ? Math.max(min, Math.trunc(parsed))
                              : min,
                          } as Partial<PoolConfig>);
                        }}
                        className="h-8 text-sm"
                      />
                      <p className="text-[11px] text-muted-foreground">
                        {t(`proxyPool.advanced.${key}Hint`)}
                      </p>
                    </div>
                  ))}
                </div>

                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                  <div className="space-y-1.5">
                    <Label htmlFor="pp-egressProbeUrl" className="text-xs">
                      {t("proxyPool.advanced.egressProbeUrl")}
                    </Label>
                    <Input
                      id="pp-egressProbeUrl"
                      value={draft.egressProbeUrl}
                      onChange={(e) =>
                        updateDraft({ egressProbeUrl: e.target.value })
                      }
                      className="h-8 font-mono text-sm"
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="pp-latencyProbeUrl" className="text-xs">
                      {t("proxyPool.advanced.latencyProbeUrl")}
                    </Label>
                    <Input
                      id="pp-latencyProbeUrl"
                      value={draft.latencyProbeUrl}
                      onChange={(e) =>
                        updateDraft({ latencyProbeUrl: e.target.value })
                      }
                      className="h-8 font-mono text-sm"
                    />
                  </div>
                </div>

                <Button
                  size="sm"
                  onClick={handleSaveConfig}
                  disabled={!dirty || setPoolConfig.isPending}
                >
                  {setPoolConfig.isPending && (
                    <Loader2 className="mr-2 h-3.5 w-3.5 animate-spin" />
                  )}
                  {t("common.save")}
                </Button>
              </div>
            ) : (
              <div className="flex justify-center py-4">
                <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
              </div>
            )}
          </AccordionContent>
        </AccordionItem>

        <AccordionItem value="leases">
          <AccordionTrigger className="text-sm font-semibold">
            {t("proxyPool.leases.title", { count: leases.length })}
          </AccordionTrigger>
          <AccordionContent>
            <div className="space-y-2 pt-1">
              <div className="flex items-center justify-between gap-2">
                <p className="text-xs text-muted-foreground">
                  {t("proxyPool.leases.hint")}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setClearAllOpen(true)}
                  disabled={leases.length === 0 || clearAllLeases.isPending}
                >
                  {t("proxyPool.leases.clearAll")}
                </Button>
              </div>

              {leases.length === 0 ? (
                <p className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-sm text-muted-foreground">
                  {t("proxyPool.leases.empty")}
                </p>
              ) : (
                <ul className="divide-y divide-border rounded-lg border border-border">
                  {leases.map((lease) => {
                    const node = nodeByHash.get(lease.nodeHash);
                    return (
                      <li
                        key={lease.stickyKey}
                        className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-sm"
                      >
                        <span
                          className="min-w-0 flex-1 truncate font-medium"
                          title={lease.stickyKey}
                        >
                          {lease.stickyKey}
                        </span>
                        <span className="truncate font-mono text-xs text-muted-foreground">
                          {node
                            ? `${node.tag || node.host}:${node.port}`
                            : lease.nodeHash.slice(0, 12)}
                        </span>
                        <span className="font-mono text-xs text-muted-foreground">
                          {lease.egressIp ?? "-"}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          {formatTimestamp(lease.lastUsedAtMs)}
                        </span>
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          disabled={clearLease.isPending}
                          onClick={() => clearLease.mutate(lease.stickyKey)}
                          aria-label={t("proxyPool.leases.clearFor", {
                            key: lease.stickyKey,
                          })}
                          title={t("proxyPool.leases.clearOne")}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </Button>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          </AccordionContent>
        </AccordionItem>
      </Accordion>

      <ProxyPoolSubscriptionModal
        open={formOpen}
        pending={addSub.isPending}
        onSave={(payload) =>
          addSub.mutate(payload, { onSuccess: () => setFormOpen(false) })
        }
        onCancel={() => setFormOpen(false)}
      />

      <ConfirmDialog
        isOpen={deleteTarget !== null}
        title={t("proxyPool.subscriptions.deleteTitle")}
        message={t("proxyPool.subscriptions.deleteMessage", {
          name: deleteTarget?.name ?? "",
        })}
        variant="destructive"
        pending={deleteSub.isPending}
        onConfirm={() => {
          if (deleteTarget) {
            deleteSub.mutate(deleteTarget.id, {
              onSuccess: () => setDeleteTarget(null),
            });
          }
        }}
        onCancel={() => setDeleteTarget(null)}
      />

      <ConfirmDialog
        isOpen={clearAllOpen}
        title={t("proxyPool.leases.clearAllTitle")}
        message={t("proxyPool.leases.clearAllMessage")}
        variant="destructive"
        pending={clearAllLeases.isPending}
        onConfirm={() => {
          clearAllLeases.mutate(undefined, {
            onSuccess: () => setClearAllOpen(false),
          });
        }}
        onCancel={() => setClearAllOpen(false)}
      />
    </div>
  );
}
