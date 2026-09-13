import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/clipboard";
import type { TracePayload } from "@/types/requestTrace";
import { formatBody, formatBytes } from "./traceFormatting";

export function TracePayloadView({
  payload,
  title,
}: {
  payload: TracePayload | null;
  title: string;
}) {
  const { t } = useTranslation();
  const [pretty, setPretty] = useState(true);
  if (!payload)
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {t("requestLogs.pendingResponse")}
      </p>
    );
  const copy = async () => {
    try {
      await copyText(payload.body);
      toast.success(t("requestLogs.copied"));
    } catch (error) {
      toast.error(t("requestLogs.copyFailed", { error: String(error) }));
    }
  };
  return (
    <section className="min-w-0 space-y-3" aria-label={title}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{title}</h3>
        <span className="text-xs text-muted-foreground">
          {formatBytes(payload.bodyBytes)}
        </span>
      </div>
      {payload.truncated && (
        <p
          role="status"
          className="rounded-md border border-amber-500/30 bg-amber-500/10 p-2 text-xs text-amber-700 dark:text-amber-300"
        >
          {t("requestLogs.truncated", {
            captured: formatBytes(payload.capturedBytes),
            total: formatBytes(payload.bodyBytes),
          })}
        </p>
      )}
      {payload.captureError && (
        <p className="break-words text-xs text-amber-600">
          {payload.captureError}
        </p>
      )}
      <details className="rounded-md border bg-muted/20 p-3">
        <summary className="cursor-pointer text-xs font-medium">
          {t("requestLogs.headers")} ({payload.headers.length})
        </summary>
        <dl className="mt-3 space-y-1 font-mono text-xs">
          {payload.headers.map((header, index) => (
            <div
              key={`${header.name}-${index}`}
              className="grid grid-cols-[minmax(90px,1fr)_3fr] gap-3"
            >
              <dt className="break-all text-muted-foreground">{header.name}</dt>
              <dd className="break-all">{header.value}</dd>
            </div>
          ))}
        </dl>
      </details>
      <div className="overflow-hidden rounded-md border">
        <div className="flex items-center justify-between border-b bg-muted/30 px-3 py-2">
          <span className="text-xs text-muted-foreground">
            {t("requestLogs.body")}
            {payload.redacted ? ` · ${t("requestLogs.redacted")}` : ""}
          </span>
          <div className="flex gap-2">
            <Button
              size="sm"
              variant="ghost"
              className="h-6 text-xs"
              onClick={() => setPretty(!pretty)}
            >
              {t(pretty ? "requestLogs.raw" : "requestLogs.pretty")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 gap-1 text-xs"
              onClick={copy}
              disabled={payload.bodyEncoding !== "utf8" || !payload.body}
            >
              <Copy className="h-3 w-3" />
              {t("requestLogs.copy")}
            </Button>
          </div>
        </div>
        <pre className="max-h-[45vh] overflow-auto whitespace-pre-wrap break-all p-4 font-mono text-xs leading-5 [tab-size:2]">
          {payload.bodyEncoding === "omitted"
            ? t("requestLogs.omitted")
            : formatBody(payload.body, pretty) || t("requestLogs.emptyBody")}
        </pre>
      </div>
    </section>
  );
}
