import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { ArrowRight, Boxes, Cpu, Server } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useApiEndpoints } from "@/lib/query/apiGateway";
import {
  useModelStats,
  useProviderStats,
  useUsageSummary,
} from "@/lib/query/usage";
import { useUsageEventBridge } from "@/hooks/useUsageEventBridge";
import {
  fmtInt,
  fmtUsd,
  formatTokensShort,
  getResolvedLang,
} from "@/components/usage/format";
import { cn } from "@/lib/utils";
import { ProxyStatusBar } from "./ProxyStatusBar";

interface HomePageProps {
  onOpenApiAccess: () => void;
  onOpenTraces: () => void;
  onOpenUsage: () => void;
}

const TODAY = { preset: "today" as const };
const REFRESH = { refetchInterval: 30_000 };

function share(part: number, total: number): number {
  if (total <= 0 || part <= 0) return 0;
  return Math.min(100, (part / total) * 100);
}

/**
 * 首页是运行快照，不是第二份用量面板。
 * 回答三件事：接入配了多少、今天打了多少、流量落在哪些供应商和模型。
 */
export function HomePage({
  onOpenApiAccess,
  onOpenTraces,
  onOpenUsage,
}: HomePageProps) {
  const { t, i18n } = useTranslation();
  const lang = getResolvedLang(i18n);
  useUsageEventBridge();

  const endpointsQuery = useApiEndpoints();
  const summaryQuery = useUsageSummary(TODAY, undefined, REFRESH);
  const providerQuery = useProviderStats(TODAY, undefined, REFRESH);
  const modelQuery = useModelStats(TODAY, undefined, REFRESH);

  const endpoints = endpointsQuery.data ?? [];
  const enabledEndpoints = endpoints.filter((item) => item.enabled).length;
  const keyCount = endpoints.reduce((sum, item) => sum + item.keyCount, 0);
  const summary = summaryQuery.data;
  const providers = useMemo(
    () =>
      [...(providerQuery.data ?? [])]
        .sort((a, b) => b.requestCount - a.requestCount)
        .slice(0, 6),
    [providerQuery.data],
  );
  const models = useMemo(
    () =>
      [...(modelQuery.data ?? [])]
        .sort((a, b) => b.requestCount - a.requestCount)
        .slice(0, 6),
    [modelQuery.data],
  );
  const providerTotal = providers.reduce(
    (sum, item) => sum + item.requestCount,
    0,
  );
  const modelTotal = models.reduce((sum, item) => sum + item.requestCount, 0);

  const snapshot = [
    {
      label: t("home.snapshot.endpoints"),
      value: fmtInt(enabledEndpoints, undefined, "0"),
      hint: t("home.snapshot.keys", { count: keyCount }),
    },
    {
      label: t("home.snapshot.requests"),
      value: fmtInt(summary?.totalRequests ?? 0, undefined, "0"),
      hint: t("home.snapshot.today"),
    },
    {
      label: t("home.snapshot.tokens"),
      value: formatTokensShort(summary?.realTotalTokens ?? 0, lang),
      hint: t("home.snapshot.today"),
    },
    {
      label: t("home.snapshot.cost"),
      value: fmtUsd(summary?.totalCost ?? 0, 4, "$0.0000"),
      hint: t("home.snapshot.today"),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-[1500px] flex-col gap-5 px-6 py-6">
      <ProxyStatusBar />

      <section className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        {snapshot.map((card) => (
          <div
            key={card.label}
            className="rounded-lg border border-border/60 bg-card px-4 py-3"
          >
            <p className="text-xs text-muted-foreground">{card.label}</p>
            <p className="mt-2 font-mono text-2xl font-semibold tracking-tight">
              {card.value}
            </p>
            <p className="mt-1 text-xs text-muted-foreground">{card.hint}</p>
          </div>
        ))}
      </section>

      {endpoints.length === 0 ? (
        <div className="flex min-h-40 flex-col items-center justify-center rounded-lg border border-dashed text-center">
          <Boxes className="mb-3 h-8 w-8 text-muted-foreground/50" />
          <p className="font-medium">{t("home.empty.noEndpoints")}</p>
          <p className="mt-2 max-w-md text-sm text-muted-foreground">
            {t("home.empty.noEndpointsHint")}
          </p>
          <Button className="mt-4" size="sm" onClick={onOpenApiAccess}>
            {t("home.empty.addEndpoint")}
            <ArrowRight className="ml-1.5 h-4 w-4" />
          </Button>
        </div>
      ) : (
        <section className="grid gap-4 lg:grid-cols-2">
          <DistributionCard
            icon={Server}
            title={t("home.providers")}
            empty={t("home.empty.noTraffic")}
            items={providers.map((item) => ({
              name: item.providerName,
              count: item.requestCount,
              share: share(item.requestCount, providerTotal),
            }))}
          />
          <DistributionCard
            icon={Cpu}
            title={t("home.models")}
            empty={t("home.empty.noTraffic")}
            items={models.map((item) => ({
              name: item.model,
              count: item.requestCount,
              share: share(item.requestCount, modelTotal),
            }))}
          />
        </section>
      )}

      <div className="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" onClick={onOpenApiAccess}>
          {t("nav.apiAccess")}
          <ArrowRight className="ml-1.5 h-4 w-4" />
        </Button>
        <Button variant="outline" size="sm" onClick={onOpenTraces}>
          {t("requestLogs.modeTraces")}
          <ArrowRight className="ml-1.5 h-4 w-4" />
        </Button>
        <Button variant="outline" size="sm" onClick={onOpenUsage}>
          {t("requestLogs.modeUsage")}
          <ArrowRight className="ml-1.5 h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}

function DistributionCard({
  icon: Icon,
  title,
  empty,
  items,
}: {
  icon: typeof Server;
  title: string;
  empty: string;
  items: { name: string; count: number; share: number }[];
}) {
  const { t, i18n } = useTranslation();
  const locale = i18n.resolvedLanguage || i18n.language;

  return (
    <div className="rounded-lg border border-border/60 bg-card p-4">
      <div className="mb-3 flex items-center gap-2 text-sm font-medium">
        <Icon className="h-4 w-4 text-muted-foreground" />
        {title}
      </div>
      {items.length === 0 ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          {empty}
        </p>
      ) : (
        <ul className="space-y-3">
          {items.map((item) => (
            <li key={item.name}>
              <div className="mb-1 flex items-baseline justify-between gap-3 text-sm">
                <span
                  className="min-w-0 truncate font-medium"
                  title={item.name}
                >
                  {item.name}
                </span>
                <span className="shrink-0 font-mono text-xs text-muted-foreground">
                  {fmtInt(item.count, locale)} {t("home.snapshot.requestUnit")}
                </span>
              </div>
              <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                <div
                  className={cn("h-full rounded-full bg-foreground/70")}
                  style={{ width: `${item.share}%` }}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
