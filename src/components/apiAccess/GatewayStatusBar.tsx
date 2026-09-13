import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Info, Loader2, Play, Square } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import { cn } from "@/lib/utils";
import type { UpstreamType } from "@/types/apiGateway";

export function GatewayStatusBar({
  upstream,
  actions,
}: {
  upstream: UpstreamType;
  actions?: ReactNode;
}) {
  const { t } = useTranslation();
  const {
    status,
    isRunning,
    isStarting,
    isStoppingServer,
    takeoverStatus,
    startProxyServer,
    stopProxyServer,
  } = useProxyStatus();
  const address = status?.address === "0.0.0.0" ? "127.0.0.1" : status?.address;
  const host =
    address === "::"
      ? "[::1]"
      : address?.includes(":")
        ? `[${address}]`
        : address;
  const suffix = ["codex", "openai", "deepseek"].includes(upstream)
    ? "/v1"
    : "";
  const baseUrl =
    isRunning && host && status?.port
      ? `http://${host}:${status.port}${suffix}`
      : null;

  const isBusy = isStarting || isStoppingServer;
  // 网关就是代理总开关，停掉会让已接管的 CLI 配置指向死地址。
  // 这里的应用列表必须和 stop_proxy_server 的后端校验一致：前端只是为了给出可本地化的提示，
  // 多查会误拦后端允许的操作，少查则会让用户看到后端那句未翻译的硬编码报错。
  const takeoverActive = Boolean(
    takeoverStatus?.claude ||
      takeoverStatus?.codex ||
      takeoverStatus?.gemini ||
      takeoverStatus?.grokbuild ||
      takeoverStatus?.opencode ||
      takeoverStatus?.openclaw,
  );

  const handleToggle = () => {
    if (!isRunning) {
      void startProxyServer().catch((error: unknown) =>
        console.error("[Gateway] Start failed", error),
      );
      return;
    }

    if (takeoverActive) {
      toast.warning(t("apiAccess.gateway.stopBlockedByTakeover"), {
        duration: 5000,
      });
      return;
    }

    void stopProxyServer().catch((error: unknown) =>
      console.error("[Gateway] Stop failed", error),
    );
  };

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span
          className={cn(
            "h-2 w-2 shrink-0 rounded-full",
            isRunning ? "bg-emerald-500" : "bg-muted-foreground/40",
          )}
          aria-hidden
        />
        <span className="text-sm font-medium">
          {t("apiAccess.gateway.title")}
        </span>
        {baseUrl && (
          <code
            className="min-w-0 truncate text-sm text-muted-foreground"
            aria-label={t("apiAccess.gateway.baseUrl")}
          >
            {baseUrl}
          </code>
        )}
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              className="text-muted-foreground transition-colors hover:text-foreground"
              aria-label={t("apiAccess.gateway.hint")}
            >
              <Info className="h-4 w-4" />
            </button>
          </TooltipTrigger>
          <TooltipContent side="bottom" align="start" className="max-w-md">
            {t("apiAccess.gateway.hint")}
          </TooltipContent>
        </Tooltip>

        <div className="ml-auto flex items-center gap-3">
          {actions}
          <Button
            variant="outline"
            disabled={!status || isBusy}
            onClick={handleToggle}
          >
            {isBusy ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : isRunning ? (
              <Square className="mr-2 h-4 w-4" />
            ) : (
              <Play className="mr-2 h-4 w-4" />
            )}
            {t(
              isRunning ? "apiAccess.gateway.stop" : "apiAccess.gateway.start",
            )}
          </Button>
        </div>
      </div>
    </TooltipProvider>
  );
}
