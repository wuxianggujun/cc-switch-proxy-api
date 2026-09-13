import { useState } from "react";
import { useTranslation } from "react-i18next";
import { BarChart3, ListTree } from "lucide-react";
import { cn } from "@/lib/utils";
import { TraceExplorer } from "./TraceExplorer";
import { UsagePanel } from "./UsagePanel";

/** 追踪回答“这一次请求发生了什么”，用量回答“这段时间花了多少”。 */
export type ObservabilityMode = "traces" | "usage";

const MODES: {
  id: ObservabilityMode;
  labelKey: string;
  icon: typeof ListTree;
}[] = [
  { id: "traces", labelKey: "requestLogs.modeTraces", icon: ListTree },
  { id: "usage", labelKey: "requestLogs.modeUsage", icon: BarChart3 },
];

interface RequestLogsPageProps {
  /** 从头部「使用统计」入口进入时直接落在用量模式。 */
  initialMode?: ObservabilityMode;
}

/**
 * 请求与用量。两块数据源不同（`request_traces` / `proxy_request_logs`），
 * 筛选维度也不同，所以按模式切换而不是共用一条筛选栏。
 */
export function RequestLogsPage({
  initialMode = "traces",
}: RequestLogsPageProps = {}) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<ObservabilityMode>(initialMode);

  return (
    <div
      className="mx-auto w-full max-w-[1500px] space-y-5 px-6 py-6"
      data-testid="request-logs-page"
    >
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <p className="mb-1 text-xs font-medium uppercase tracking-widest text-muted-foreground">
            {mode === "traces" ? "HTTP / SSE" : "TOKENS / COST"}
          </p>
          <p className="max-w-2xl text-sm leading-6 text-muted-foreground">
            {t(mode === "traces" ? "requestLogs.subtitle" : "usage.subtitle")}
          </p>
        </div>
        <div
          role="tablist"
          aria-label={t("requestLogs.title")}
          className="flex items-center gap-1 rounded-lg border border-border/50 bg-muted/30 p-1"
        >
          {MODES.map(({ id, labelKey, icon: Icon }) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={mode === id}
              onClick={() => setMode(id)}
              className={cn(
                "flex h-8 items-center gap-2 rounded-md px-3 text-sm font-medium transition-colors",
                mode === id
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:bg-background/60 hover:text-foreground",
              )}
            >
              <Icon className="h-4 w-4 shrink-0" />
              {t(labelKey)}
            </button>
          ))}
        </div>
      </div>
      {mode === "traces" ? <TraceExplorer /> : <UsagePanel />}
    </div>
  );
}
