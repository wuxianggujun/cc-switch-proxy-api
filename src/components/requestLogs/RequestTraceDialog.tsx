import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Copy, ArrowRight, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogClose,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button } from "@/components/ui/button";
import { useRequestTrace } from "@/lib/query/requestTraces";
import { copyText } from "@/lib/clipboard";
import { TracePayloadView } from "./TracePayloadView";
import { extractPrompts, protocolLabels } from "./traceFormatting";

function PromptView({ body }: { body: string }) {
  const { t } = useTranslation();
  const segments = extractPrompts(body);
  if (!segments?.length)
    return (
      <p className="text-sm text-muted-foreground">
        {t("requestLogs.promptsUnparsed")}
      </p>
    );
  return (
    <div className="space-y-3">
      {segments.map((segment, index) => (
        <div className="rounded-md border" key={index}>
          <div className="border-b bg-muted/30 px-3 py-1.5 font-mono text-xs font-medium">
            {segment.role}
          </div>
          <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-xs leading-5">
            {segment.text}
          </pre>
        </div>
      ))}
    </div>
  );
}

export function RequestTraceDialog({
  requestId,
  onClose,
}: {
  requestId: string | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { data, isLoading, error, refetch } = useRequestTrace(requestId);
  const [attemptIndex, setAttemptIndex] = useState(0);
  const attempt = data?.attempts[attemptIndex] ?? data?.attempts[0];
  const copy = async () => {
    if (!data) return;
    try {
      await copyText(JSON.stringify(data, null, 2));
      toast.success(t("requestLogs.copied"));
    } catch (error) {
      toast.error(t("requestLogs.copyFailed", { error: String(error) }));
    }
  };
  return (
    <Dialog
      open={!!requestId}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="flex max-h-[90vh] w-[95vw] max-w-6xl flex-col overflow-hidden p-0">
        <DialogHeader className="relative shrink-0 border-b px-6 py-4 pr-14">
          <DialogTitle>{t("requestLogs.detailTitle")}</DialogTitle>
          <DialogDescription className="break-all font-mono text-xs">
            {requestId}
          </DialogDescription>
          <DialogClose asChild>
            <Button
              variant="ghost"
              size="icon"
              className="absolute right-3 top-3 h-8 w-8"
              aria-label={t("common.close")}
            >
              <X className="h-4 w-4" />
            </Button>
          </DialogClose>
        </DialogHeader>
        <div className="min-h-0 overflow-y-auto px-6 pb-6">
          {isLoading && (
            <p className="py-8 text-muted-foreground">
              {t("requestLogs.loading")}
            </p>
          )}
          {error && (
            <div
              role="alert"
              className="space-y-2 py-4 text-sm text-destructive"
            >
              {String(error)}
              <Button variant="outline" onClick={() => refetch()}>
                {t("requestLogs.refresh")}
              </Button>
            </div>
          )}
          {!isLoading && !error && !data && (
            <p className="py-8">{t("requestLogs.noDetail")}</p>
          )}
          {data && (
            <div className="space-y-4 pt-4">
              <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
                <div className="flex items-center gap-2">
                  <span className="font-medium">
                    {protocolLabels[data.entryProtocol] ?? data.entryProtocol}
                  </span>
                  <ArrowRight className="h-4 w-4 text-muted-foreground" />
                  <span>{data.providerName ?? "—"}</span>
                </div>
                <Button size="sm" variant="outline" onClick={copy}>
                  <Copy className="mr-2 h-3 w-3" />
                  {t("requestLogs.copyTrace")}
                </Button>
              </div>
              <dl className="grid grid-cols-2 gap-3 rounded-lg border bg-muted/20 p-4 text-xs md:grid-cols-4">
                {[
                  [
                    t("requestLogs.clientIp"),
                    `${data.clientIp.includes(":") ? `[${data.clientIp}]` : data.clientIp}${data.clientPort ? `:${data.clientPort}` : ""}`,
                  ],
                  [
                    t("requestLogs.status"),
                    `${data.statusCode ?? "—"} · ${t(`requestLogs.states.${data.state}`)}`,
                  ],
                  [
                    t("requestLogs.duration"),
                    data.durationMs == null ? "—" : `${data.durationMs} ms`,
                  ],
                  [
                    t("requestLogs.firstByte"),
                    data.firstByteMs == null ? "—" : `${data.firstByteMs} ms`,
                  ],
                  [t("requestLogs.model"), data.model ?? "—"],
                  [
                    t("requestLogs.startedAt"),
                    new Date(data.startedAt).toLocaleString(),
                  ],
                  [t("requestLogs.request"), `${data.method} ${data.path}`],
                  [t("requestLogs.attempts"), String(data.attemptCount)],
                ].map(([label, value]) => (
                  <div key={label}>
                    <dt className="text-muted-foreground">{label}</dt>
                    <dd className="mt-1 break-all font-mono">{value}</dd>
                  </div>
                ))}
              </dl>
              {data.error && (
                <p
                  role="alert"
                  className="whitespace-pre-wrap break-words rounded-md border border-destructive/25 bg-destructive/5 p-3 text-sm text-destructive"
                >
                  {data.error}
                </p>
              )}
              <p className="text-xs text-muted-foreground">
                {t("requestLogs.peerHint")}
              </p>
              <Tabs defaultValue="original">
                <TabsList className="mb-4 flex h-auto flex-wrap justify-start gap-1">
                  <TabsTrigger value="original">
                    {t("requestLogs.originalRequest")}
                  </TabsTrigger>
                  <TabsTrigger value="attempts">
                    {t("requestLogs.upstreamAttempts")} ({data.attempts.length})
                  </TabsTrigger>
                  <TabsTrigger value="response">
                    {t("requestLogs.finalResponse")}
                  </TabsTrigger>
                  <TabsTrigger value="prompts">
                    {t("requestLogs.prompts")}
                  </TabsTrigger>
                </TabsList>
                <TabsContent value="original">
                  <TracePayloadView
                    payload={data.request}
                    title={t("requestLogs.originalRequest")}
                  />
                </TabsContent>
                <TabsContent value="response">
                  <TracePayloadView
                    payload={data.response}
                    title={t("requestLogs.finalResponse")}
                  />
                </TabsContent>
                <TabsContent value="attempts" className="space-y-4">
                  {data.attempts.length === 0 && (
                    <p className="text-sm text-muted-foreground">
                      {t("requestLogs.noAttempts")}
                    </p>
                  )}
                  <div className="flex flex-wrap gap-2">
                    {data.attempts.map((item, index) => (
                      <Button
                        key={item.index}
                        size="sm"
                        variant={attempt === item ? "default" : "outline"}
                        onClick={() => setAttemptIndex(index)}
                      >
                        #{item.index} · {item.providerName} ·{" "}
                        {item.statusCode ?? "—"}
                      </Button>
                    ))}
                  </div>
                  {attempt && (
                    <div className="space-y-4">
                      <div className="space-y-1 rounded-md border bg-muted/20 p-3 font-mono text-xs">
                        <p className="break-all">
                          {attempt.method} {attempt.url}
                        </p>
                        <p>
                          {protocolLabels[attempt.protocol] ?? attempt.protocol}{" "}
                          · {attempt.model} · {attempt.durationMs ?? "—"} ms
                        </p>
                        <p className="break-all">
                          {t("requestLogs.outboundProxy")}:{" "}
                          {attempt.proxy ?? t("requestLogs.direct")}
                        </p>
                      </div>
                      {attempt.error && (
                        <p className="whitespace-pre-wrap break-words text-sm text-destructive">
                          {attempt.error}
                        </p>
                      )}
                      <TracePayloadView
                        payload={attempt.request}
                        title={t("requestLogs.requestBody")}
                      />
                      <TracePayloadView
                        payload={attempt.response}
                        title={t("requestLogs.upstreamResponse")}
                      />
                    </div>
                  )}
                </TabsContent>
                <TabsContent value="prompts" className="space-y-4">
                  <p className="text-xs text-muted-foreground">
                    {t("requestLogs.compareHint")}
                  </p>
                  {data.attempts.length > 1 && (
                    <select
                      className="rounded-md border bg-background p-2 text-sm"
                      aria-label={t("requestLogs.upstreamAttempts")}
                      value={attemptIndex}
                      onChange={(event) =>
                        setAttemptIndex(Number(event.target.value))
                      }
                    >
                      {data.attempts.map((item, index) => (
                        <option key={item.index} value={index}>
                          #{item.index} {item.providerName}
                        </option>
                      ))}
                    </select>
                  )}
                  <div className="grid min-w-0 gap-4 md:grid-cols-2">
                    <section className="min-w-0">
                      <h3 className="mb-3 text-sm font-medium">
                        {t("requestLogs.originalRequest")}
                      </h3>
                      <PromptView body={data.request.body} />
                    </section>
                    <section className="min-w-0">
                      <h3 className="mb-3 text-sm font-medium">
                        {t("requestLogs.requestBody")}
                      </h3>
                      <PromptView body={attempt?.request.body ?? ""} />
                    </section>
                  </div>
                </TabsContent>
              </Tabs>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
