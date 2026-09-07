import { useTranslation } from "react-i18next";
import { Activity, CircleDot, Power } from "lucide-react";
import { useProxyStatusQuery } from "@/lib/query/proxy";
import { getAppLabel } from "@/config/appConfig";
import { cn } from "@/lib/utils";

/**
 * 首页顶部代理运行态条。只读展示，控制入口仍在设置 → 代理。
 *
 * active_targets 是运行时观测值（请求成功后回填），不是配置，所以未发生请求时
 * 为空属正常状态。
 */
export function ProxyStatusBar() {
  const { t } = useTranslation();
  const { data: status } = useProxyStatusQuery();

  const running = status?.running ?? false;
  const targets = status?.active_targets ?? [];

  return (
    <div className="flex flex-wrap items-center gap-x-6 gap-y-2 rounded-lg border border-border/60 bg-card px-4 py-3">
      <div className="flex items-center gap-2">
        <span
          className={cn(
            "flex h-2 w-2 rounded-full",
            running ? "bg-emerald-500" : "bg-muted-foreground/40",
          )}
        />
        <span className="text-sm font-medium">
          {running
            ? t("home.proxy.running", { defaultValue: "代理运行中" })
            : t("home.proxy.stopped", { defaultValue: "代理未运行" })}
        </span>
        {running && status ? (
          <span className="font-mono text-xs text-muted-foreground">
            {status.address}:{status.port}
          </span>
        ) : null}
      </div>

      {running && status ? (
        <>
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Activity className="h-3.5 w-3.5" />
            <span>
              {t("home.proxy.requests", {
                total: status.total_requests,
                defaultValue: "{{total}} 次请求",
              })}
            </span>
            <span className="text-muted-foreground/50">·</span>
            <span>
              {t("home.proxy.successRate", {
                rate: Math.round(status.success_rate * 100) / 100,
                defaultValue: "成功率 {{rate}}%",
              })}
            </span>
          </div>

          {targets.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              <CircleDot className="h-3.5 w-3.5 text-muted-foreground" />
              {targets.map((target) => (
                <span
                  key={`${target.app_type}:${target.provider_id}`}
                  className="rounded bg-muted px-2 py-0.5 text-xs text-muted-foreground"
                >
                  {getAppLabel(target.app_type)} → {target.provider_name}
                </span>
              ))}
            </div>
          ) : null}
        </>
      ) : (
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Power className="h-3.5 w-3.5" />
          {t("home.proxy.startHint", {
            defaultValue: "在设置 → 代理中启动服务后即可记录请求",
          })}
        </span>
      )}
    </div>
  );
}
