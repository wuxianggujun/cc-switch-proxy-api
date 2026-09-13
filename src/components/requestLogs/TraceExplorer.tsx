import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  Activity,
  ChevronLeft,
  ChevronRight,
  FileText,
  Pause,
  Play,
  RefreshCw,
  Search,
  Settings2,
  Trash2,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  useClearRequestTraces,
  useRequestTraceConfig,
  useRequestTraces,
  useSaveRequestTraceConfig,
} from "@/lib/query/requestTraces";
import type { RequestTraceFilters } from "@/types/requestTrace";
import { RequestTraceDialog } from "./RequestTraceDialog";
import { RequestLogSettings } from "./RequestLogSettings";
import { formatBytes, protocolLabels } from "./traceFormatting";

/**
 * 逐请求诊断视图，读 `request_traces`。与用量统计（读 `proxy_request_logs`）
 * 同页不同源：这里回答“这一次请求发生了什么”，用量回答“这段时间花了多少”。
 */
export function TraceExplorer() {
  const { t } = useTranslation();
  const [live, setLive] = useState(true);
  const [page, setPage] = useState(0);
  const [queryInput, setQueryInput] = useState("");
  const [ipInput, setIpInput] = useState("");
  const [query, setQuery] = useState("");
  const [clientIp, setClientIp] = useState("");
  const [entryProtocol, setEntryProtocol] = useState("");
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [statusCode, setStatusCode] = useState("");
  const [range, setRange] = useState(0);
  const [startTime, setStartTime] = useState<number>();
  const [requestId, setRequestId] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [clearOpen, setClearOpen] = useState(false);
  const filters = useMemo<RequestTraceFilters>(
    () => ({
      query: query || undefined,
      clientIp: clientIp || undefined,
      entryProtocol: entryProtocol || undefined,
      errorsOnly,
      statusCode: statusCode ? Number(statusCode) : undefined,
      startTime,
    }),
    [query, clientIp, entryProtocol, errorsOnly, statusCode, startTime],
  );
  const logs = useRequestTraces(filters, page, live);
  const config = useRequestTraceConfig();
  const save = useSaveRequestTraceConfig();
  const clear = useClearRequestTraces();
  const totalPages = Math.max(1, Math.ceil((logs.data?.total ?? 0) / 25));
  useEffect(() => {
    if (logs.data && page >= totalPages) setPage(totalPages - 1);
  }, [logs.data, page, totalPages]);
  const selectClass =
    "h-9 rounded-md border border-input bg-background px-2 text-xs";
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-card p-4">
        <div className="flex flex-wrap items-center gap-4 text-xs text-muted-foreground">
          <Activity className="h-4 w-4 text-primary" />
          <span>
            {t("requestLogs.total", { count: logs.data?.total ?? 0 })}
          </span>
          <span>
            {t("requestLogs.storage", {
              size: formatBytes(logs.data?.storageBytes ?? 0),
            })}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="mr-2 flex items-center gap-3 text-sm">
            {t("requestLogs.enabled")}
            <Switch
              aria-label={t("requestLogs.enabled")}
              checked={config.data?.enabled ?? false}
              disabled={!config.data || save.isPending}
              onCheckedChange={async (enabled) => {
                if (!config.data) return;
                try {
                  await save.mutateAsync({ ...config.data, enabled });
                } catch (error) {
                  toast.error(String(error));
                }
              }}
            />
          </label>
          <Button variant="outline" size="sm" onClick={() => setLive(!live)}>
            {live ? (
              <Pause className="mr-1.5 h-3.5 w-3.5" />
            ) : (
              <Play className="mr-1.5 h-3.5 w-3.5" />
            )}
            {t(live ? "requestLogs.live" : "requestLogs.paused")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => logs.refetch()}
            disabled={logs.isFetching}
          >
            <RefreshCw
              className={`mr-1.5 h-3.5 w-3.5 ${logs.isFetching ? "animate-spin" : ""}`}
            />
            {t("requestLogs.refresh")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={!config.data}
            onClick={() => setSettingsOpen(true)}
          >
            <Settings2 className="mr-1.5 h-3.5 w-3.5" />
            {t("requestLogs.settings")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setClearOpen(true)}
            disabled={clear.isPending}
          >
            <Trash2 className="mr-1.5 h-3.5 w-3.5" />
            {t("requestLogs.clear")}
          </Button>
        </div>
      </div>
      {config.data && !config.data.enabled && (
        <p
          role="status"
          className="rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-sm"
        >
          {t("requestLogs.recordingOff")}
        </p>
      )}
      {config.error && (
        <p role="alert" className="text-sm text-destructive">
          {String(config.error)}
        </p>
      )}
      <form
        className="flex flex-wrap items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          setQuery(queryInput.trim());
          setClientIp(ipInput.trim());
          setPage(0);
        }}
      >
        <Input
          aria-label={t("requestLogs.searchPlaceholder")}
          placeholder={t("requestLogs.searchPlaceholder")}
          value={queryInput}
          onChange={(e) => setQueryInput(e.target.value)}
          maxLength={512}
          className="h-9 min-w-56 flex-1 text-xs"
        />
        <Input
          aria-label={t("requestLogs.ipPlaceholder")}
          placeholder={t("requestLogs.ipPlaceholder")}
          value={ipInput}
          onChange={(e) => setIpInput(e.target.value)}
          className="h-9 w-36 text-xs"
        />
        <Button type="submit" variant="secondary" size="sm">
          <Search className="mr-1 h-3.5 w-3.5" />
          {t("requestLogs.search")}
        </Button>
        <select
          aria-label={t("requestLogs.allProtocols")}
          value={entryProtocol}
          className={selectClass}
          onChange={(e) => {
            setEntryProtocol(e.target.value);
            setPage(0);
          }}
        >
          <option value="">{t("requestLogs.allProtocols")}</option>
          {Object.entries(protocolLabels).map(([key, label]) => (
            <option key={key} value={key}>
              {label}
            </option>
          ))}
        </select>
        <select
          aria-label={t("requestLogs.status")}
          value={statusCode}
          className={selectClass}
          onChange={(e) => {
            setStatusCode(e.target.value);
            setPage(0);
          }}
        >
          <option value="">{t("requestLogs.status")}</option>
          {[200, 400, 401, 403, 404, 413, 422, 429, 500, 502, 503, 504].map(
            (code) => (
              <option key={code} value={code}>
                {code}
              </option>
            ),
          )}
        </select>
        <select
          aria-label={t("requestLogs.timeRange")}
          value={range}
          className={selectClass}
          onChange={(e) => {
            const seconds = Number(e.target.value);
            setRange(seconds);
            setStartTime(seconds ? Date.now() - seconds * 1000 : undefined);
            setPage(0);
          }}
        >
          <option value={0}>{t("requestLogs.allTime")}</option>
          <option value={3600}>{t("requestLogs.lastHour")}</option>
          <option value={86400}>{t("requestLogs.lastDay")}</option>
          <option value={604800}>{t("requestLogs.lastWeek")}</option>
        </select>
        <label className="flex items-center gap-2 px-1 text-xs">
          <input
            type="checkbox"
            checked={errorsOnly}
            onChange={(e) => {
              setErrorsOnly(e.target.checked);
              setPage(0);
            }}
          />
          {t("requestLogs.errorsOnly")}
        </label>
      </form>
      {logs.error ? (
        <div
          role="alert"
          className="rounded-lg border border-destructive/30 p-5 text-sm text-destructive"
        >
          {String(logs.error)}
        </div>
      ) : logs.isLoading ? (
        <div className="flex h-56 items-center justify-center text-muted-foreground">
          {t("requestLogs.loading")}
        </div>
      ) : !logs.data?.data.length ? (
        <div className="flex min-h-64 flex-col items-center justify-center rounded-lg border border-dashed text-center">
          <FileText className="mb-3 h-8 w-8 text-muted-foreground/50" />
          <h2 className="font-medium">{t("requestLogs.empty")}</h2>
          <p className="mt-2 max-w-md text-sm text-muted-foreground">
            {t("requestLogs.emptyHint")}
          </p>
        </div>
      ) : (
        <div className="overflow-x-auto rounded-lg border bg-card">
          <Table>
            <TableHeader>
              <TableRow>
                {[
                  "startedAt",
                  "request",
                  "clientIp",
                  "model",
                  "provider",
                  "status",
                  "duration",
                  "attempts",
                ].map((key) => (
                  <TableHead key={key} className="whitespace-nowrap text-xs">
                    {t(`requestLogs.${key}`)}
                  </TableHead>
                ))}
              </TableRow>
            </TableHeader>
            <TableBody>
              {logs.data.data.map((item) => (
                <TableRow
                  key={item.requestId}
                  className="cursor-pointer"
                  onClick={() => setRequestId(item.requestId)}
                >
                  <TableCell className="whitespace-nowrap text-xs">
                    {new Date(item.startedAt).toLocaleTimeString()}
                    <div className="mt-1 text-[10px] text-muted-foreground">
                      {new Date(item.startedAt).toLocaleDateString()}
                    </div>
                  </TableCell>
                  <TableCell className="max-w-60">
                    <button
                      className="max-w-full text-left focus-visible:outline-primary"
                      onClick={(e) => {
                        e.stopPropagation();
                        setRequestId(item.requestId);
                      }}
                    >
                      <span className="block truncate font-mono text-xs">
                        {item.method} {item.path}
                      </span>
                      <span className="mt-1 block text-[10px] text-muted-foreground">
                        {protocolLabels[item.entryProtocol] ??
                          item.entryProtocol}
                      </span>
                    </button>
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {item.clientIp}
                  </TableCell>
                  <TableCell
                    className="max-w-48 truncate font-mono text-xs"
                    title={item.model ?? ""}
                  >
                    {item.model ?? "—"}
                  </TableCell>
                  <TableCell
                    className="max-w-40 truncate text-xs"
                    title={item.providerName ?? ""}
                  >
                    {item.providerName ?? "—"}
                  </TableCell>
                  <TableCell>
                    <span
                      className={`inline-flex whitespace-nowrap rounded border px-2 py-0.5 text-[11px] ${item.state === "completed" ? "border-emerald-500/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300" : item.state === "in_progress" ? "border-blue-500/20 bg-blue-500/10 text-blue-600 dark:text-blue-300" : "border-destructive/20 bg-destructive/5 text-destructive"}`}
                    >
                      {item.statusCode ?? "…"} ·{" "}
                      {t(`requestLogs.states.${item.state}`)}
                    </span>
                  </TableCell>
                  <TableCell className="whitespace-nowrap font-mono text-xs">
                    {item.durationMs == null ? "—" : `${item.durationMs} ms`}
                  </TableCell>
                  <TableCell className="text-center font-mono text-xs">
                    {item.attemptCount}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
        <p>{t("requestLogs.credentialsHint")}</p>
        <div className="flex shrink-0 items-center gap-2">
          <Button
            size="icon"
            variant="outline"
            className="h-8 w-8"
            aria-label={t("requestLogs.previous")}
            disabled={page === 0}
            onClick={() => setPage(page - 1)}
          >
            <ChevronLeft className="h-4 w-4" />
          </Button>
          <span>
            {page + 1} / {totalPages}
          </span>
          <Button
            size="icon"
            variant="outline"
            className="h-8 w-8"
            aria-label={t("requestLogs.next")}
            disabled={page + 1 >= totalPages}
            onClick={() => setPage(page + 1)}
          >
            <ChevronRight className="h-4 w-4" />
          </Button>
        </div>
      </div>
      <RequestTraceDialog
        key={requestId}
        requestId={requestId}
        onClose={() => setRequestId(null)}
      />
      {settingsOpen && config.data && (
        <RequestLogSettings
          config={config.data}
          onClose={() => setSettingsOpen(false)}
        />
      )}
      <ConfirmDialog
        isOpen={clearOpen}
        title={t("requestLogs.clearTitle")}
        message={t("requestLogs.clearMessage")}
        pending={clear.isPending}
        onCancel={() => setClearOpen(false)}
        onConfirm={async () => {
          try {
            const count = await clear.mutateAsync();
            setRequestId(null);
            setPage(0);
            setClearOpen(false);
            toast.success(t("requestLogs.clearDone", { count }));
          } catch (error) {
            toast.error(String(error));
          }
        }}
      />
    </div>
  );
}
