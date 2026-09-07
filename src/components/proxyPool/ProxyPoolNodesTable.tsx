/**
 * 节点列表
 *
 * 订阅动辄几千个节点，用 @tanstack/react-virtual 虚拟化行；
 * 因此这里不用 <table>，而是 role="grid" 的 div 网格 —— 绝对定位的行
 * 放不进 tbody 的布局模型里。
 */

import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { RotateCcw, Search, Zap } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { NodeView } from "@/lib/api/proxyPool";

const ROW_HEIGHT = 44;

/** 列宽共用一套 grid 模板，表头与行才不会错位 */
const GRID_COLS =
  "grid grid-cols-[minmax(0,1.6fr)_72px_minmax(0,1.4fr)_minmax(0,1fr)_80px_96px_88px] items-center gap-3";

interface ProxyPoolNodesTableProps {
  nodes: NodeView[];
  probingHashes: Set<string>;
  onProbeNode: (hash: string) => void;
  onResetCircuit: (hash: string) => void;
}

export function ProxyPoolNodesTable({
  nodes,
  probingHashes,
  onProbeNode,
  onResetCircuit,
}: ProxyPoolNodesTableProps) {
  const { t } = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  const [filter, setFilter] = useState("");

  const filtered = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return nodes;
    return nodes.filter(
      (node) =>
        node.tag.toLowerCase().includes(needle) ||
        node.host.toLowerCase().includes(needle) ||
        node.protocol.includes(needle) ||
        (node.health.egressIp ?? "").includes(needle),
    );
  }, [nodes, filter]);

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex items-center gap-2">
        <div className="relative max-w-xs flex-1">
          <Search
            className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder={t("proxyPool.nodes.filterPlaceholder")}
            aria-label={t("proxyPool.nodes.filterPlaceholder")}
            className="h-8 pl-8 text-sm"
          />
        </div>
        <span className="text-xs text-muted-foreground">
          {t("proxyPool.nodes.shown", {
            shown: filtered.length,
            total: nodes.length,
          })}
        </span>
      </div>

      <div className="rounded-lg border border-border">
        <div
          className={cn(
            GRID_COLS,
            "border-b border-border bg-muted/40 px-3 py-2 text-xs font-medium text-muted-foreground",
          )}
        >
          <span>{t("proxyPool.nodes.tag")}</span>
          <span>{t("proxyPool.nodes.protocol")}</span>
          <span>{t("proxyPool.nodes.endpoint")}</span>
          <span>{t("proxyPool.nodes.egressIp")}</span>
          <span>{t("proxyPool.nodes.latency")}</span>
          <span>{t("proxyPool.nodes.status")}</span>
          <span className="text-right">{t("common.actions")}</span>
        </div>

        {filtered.length === 0 ? (
          <p className="px-3 py-8 text-center text-sm text-muted-foreground">
            {nodes.length === 0
              ? t("proxyPool.nodes.empty")
              : t("proxyPool.nodes.noMatch")}
          </p>
        ) : (
          <div
            ref={scrollRef}
            className="max-h-[420px] overflow-y-auto"
            role="grid"
            aria-label={t("proxyPool.nodes.title")}
            aria-rowcount={filtered.length}
          >
            <div
              style={{
                height: virtualizer.getTotalSize(),
                position: "relative",
              }}
            >
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const node = filtered[virtualRow.index];
                const broken = node.health.circuitOpenSinceMs > 0;
                const probing = probingHashes.has(node.hash);
                return (
                  <div
                    key={node.hash}
                    role="row"
                    aria-rowindex={virtualRow.index + 1}
                    className={cn(
                      GRID_COLS,
                      "absolute left-0 top-0 w-full border-b border-border/50 px-3 text-sm last:border-b-0",
                    )}
                    style={{
                      height: ROW_HEIGHT,
                      transform: `translateY(${virtualRow.start}px)`,
                    }}
                  >
                    <span className="truncate" title={node.tag}>
                      {node.tag || "-"}
                    </span>
                    <span className="text-xs uppercase text-muted-foreground">
                      {node.protocol}
                    </span>
                    <span
                      className="truncate font-mono text-xs"
                      title={`${node.host}:${node.port}`}
                    >
                      {node.host}:{node.port}
                    </span>
                    <span className="truncate font-mono text-xs text-muted-foreground">
                      {node.health.egressIp ?? "-"}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {/* 未探测出有效值时 latencyEwmaMs 为 undefined */}
                      {node.health.latencyEwmaMs === undefined
                        ? "-"
                        : `${Math.round(node.health.latencyEwmaMs)} ms`}
                    </span>
                    <span>
                      {broken ? (
                        <Badge variant="destructive" className="text-[10px]">
                          {t("proxyPool.nodes.circuitOpen")}
                        </Badge>
                      ) : (
                        <Badge variant="secondary" className="text-[10px]">
                          {t("proxyPool.nodes.healthy")}
                        </Badge>
                      )}
                    </span>
                    <span className="flex justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7"
                        disabled={probing}
                        onClick={() => onProbeNode(node.hash)}
                        aria-label={t("proxyPool.nodes.probeOne", {
                          tag: node.tag || node.host,
                        })}
                        title={t("proxyPool.nodes.probeOneShort")}
                      >
                        <Zap className="h-3.5 w-3.5" />
                      </Button>
                      {broken && (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => onResetCircuit(node.hash)}
                          aria-label={t("proxyPool.nodes.resetCircuitFor", {
                            tag: node.tag || node.host,
                          })}
                          title={t("proxyPool.nodes.resetCircuit")}
                        >
                          <RotateCcw className="h-3.5 w-3.5" />
                        </Button>
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
