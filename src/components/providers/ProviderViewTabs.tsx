import { useTranslation } from "react-i18next";
import type { AppId } from "@/lib/api";
import { cn } from "@/lib/utils";
import { getProviderTabs, type NavViewId } from "@/config/navConfig";

interface ProviderViewTabsProps {
  /** 已折叠 claude-desktop → claude，用于标签可见性判定。 */
  featureApp: AppId;
  currentView: NavViewId;
  onSelectView: (view: NavViewId) => void;
  /** 面板有未完成操作时禁止切换。 */
  disabled?: boolean;
}

/**
 * 供应商页顶部标签栏。
 * Skills / 提示词 / 会话 / MCP 都以「当前供应商应用」为上下文，
 * 统一供应商编辑的也是供应商，因此收在这里而不是各占一行侧边栏。
 */
export function ProviderViewTabs({
  featureApp,
  currentView,
  onSelectView,
  disabled = false,
}: ProviderViewTabsProps) {
  const { t } = useTranslation();
  const tabs = getProviderTabs(featureApp);

  if (tabs.length <= 1) return null;

  return (
    <nav
      aria-label={t("nav.providers")}
      className="flex items-center gap-1 overflow-x-auto rounded-lg border border-border/50 bg-muted/30 p-1"
    >
      {tabs.map(({ id, labelKey, icon: Icon }) => {
        const isCurrent = currentView === id;
        return (
          <button
            key={id}
            type="button"
            onClick={() => onSelectView(id)}
            disabled={disabled && !isCurrent}
            aria-current={isCurrent ? "page" : undefined}
            className={cn(
              "flex h-8 shrink-0 items-center gap-2 rounded-md px-3 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-40",
              isCurrent
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:bg-background/60 hover:text-foreground",
            )}
          >
            <Icon className="h-4 w-4 shrink-0" />
            {t(labelKey)}
          </button>
        );
      })}
    </nav>
  );
}
