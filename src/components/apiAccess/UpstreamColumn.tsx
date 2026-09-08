import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { ProviderIcon } from "@/components/ProviderIcon";
import {
  UPSTREAM_LABELS,
  UPSTREAM_ORDER,
  type ApiEndpoint,
  type UpstreamType,
} from "@/types/apiGateway";

/** 各上游类型的接入数量。未出现的类型计 0。 */
function countByUpstream(
  endpoints: ApiEndpoint[],
): Record<UpstreamType, number> {
  const counts = {} as Record<UpstreamType, number>;
  for (const type of UPSTREAM_ORDER) {
    counts[type] = 0;
  }
  for (const endpoint of endpoints) {
    counts[endpoint.upstreamType] = (counts[endpoint.upstreamType] ?? 0) + 1;
  }
  return counts;
}

/** 复用 provider 图标库：这些名字在 BrandIcons 里已有对应图标。 */
const UPSTREAM_ICONS: Record<UpstreamType, string> = {
  codex: "openai",
  openai: "openai",
  deepseek: "deepseek",
  claude: "claude",
  gemini: "gemini",
};

interface UpstreamColumnProps {
  endpoints: ApiEndpoint[];
  selected: UpstreamType;
  onSelect: (type: UpstreamType) => void;
}

export function UpstreamColumn({
  endpoints,
  selected,
  onSelect,
}: UpstreamColumnProps) {
  const { t } = useTranslation();
  const counts = countByUpstream(endpoints);

  return (
    <nav
      aria-label={t("apiAccess.subtitle")}
      className="w-64 shrink-0 self-start rounded-xl border border-border/60 bg-card p-2"
    >
      <ul className="flex flex-col gap-1">
        {UPSTREAM_ORDER.map((type) => {
          const isSelected = type === selected;
          return (
            <li key={type}>
              <button
                type="button"
                onClick={() => onSelect(type)}
                aria-current={isSelected ? "true" : undefined}
                className={cn(
                  "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors",
                  isSelected
                    ? "bg-accent text-accent-foreground"
                    : "hover:bg-accent/50",
                )}
              >
                <ProviderIcon
                  icon={UPSTREAM_ICONS[type]}
                  name={UPSTREAM_LABELS[type]}
                  size={20}
                />
                <span className="flex-1 truncate text-sm font-medium">
                  {UPSTREAM_LABELS[type]}
                </span>
                <span
                  className={cn(
                    "text-sm tabular-nums",
                    counts[type] > 0
                      ? "font-semibold"
                      : "text-muted-foreground/60",
                  )}
                >
                  {counts[type]}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
