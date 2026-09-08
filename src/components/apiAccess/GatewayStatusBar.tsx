import { useTranslation } from "react-i18next";
import { Loader2, Play } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import type { UpstreamType } from "@/types/apiGateway";

export function GatewayStatusBar({ upstream }: { upstream: UpstreamType }) {
  const { t } = useTranslation();
  const { status, isRunning, isStarting, startProxyServer } = useProxyStatus();
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

  return (
    <section className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border/60 bg-card px-4 py-3">
      <div className="min-w-0 space-y-1">
        <p className="text-sm font-medium">{t("apiAccess.gateway.title")}</p>
        {baseUrl && (
          <code
            className="block break-all text-sm"
            aria-label={t("apiAccess.gateway.baseUrl")}
          >
            {baseUrl}
          </code>
        )}
        <p className="max-w-3xl text-xs text-muted-foreground">
          {t("apiAccess.gateway.hint")}
        </p>
      </div>
      <Button
        variant="outline"
        disabled={isRunning || !status || isStarting}
        onClick={() => {
          // The mutation reports failures to the user; keep the rejected promise
          // handled here as well so it cannot become an unhandled rejection.
          void startProxyServer().catch((error: unknown) =>
            console.error("[Gateway] Start failed", error),
          );
        }}
      >
        {isStarting ? (
          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
        ) : (
          <Play className="mr-2 h-4 w-4" />
        )}
        {t(isRunning ? "apiAccess.gateway.running" : "apiAccess.gateway.start")}
      </Button>
    </section>
  );
}
