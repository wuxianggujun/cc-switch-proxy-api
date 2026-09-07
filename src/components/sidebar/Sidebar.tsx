import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronRight,
  Monitor,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Terminal,
} from "lucide-react";
import type { AppId } from "@/lib/api";
import type { VisibleApps } from "@/types";
import { ProviderIcon } from "@/components/ProviderIcon";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { APP_ICON_NAME, APP_IDS, getAppLabel } from "@/config/appConfig";
import { NAV_ITEMS, getAppsForView, type NavViewId } from "@/config/navConfig";

const APP_BADGE_ICON: Partial<
  Record<AppId, { icon: typeof Terminal; offsetY?: number }>
> = {
  claude: { icon: Terminal },
  "claude-desktop": { icon: Monitor, offsetY: 0.5 },
};

/** 应用图标 + 角标（Claude Code / Desktop 用角标区分终端与桌面） */
function AppGlyph({
  app,
  size = 16,
  isActive,
}: {
  app: AppId;
  size?: number;
  isActive: boolean;
}) {
  const badgeConfig = APP_BADGE_ICON[app];
  const BadgeIcon = badgeConfig?.icon;
  return (
    // aria-hidden：图标自带 title，与相邻的应用名标签重复
    <span className="relative inline-flex shrink-0" aria-hidden="true">
      <ProviderIcon
        icon={APP_ICON_NAME[app]}
        name={getAppLabel(app)}
        size={size}
      />
      {BadgeIcon && (
        <span
          className={cn(
            "absolute -bottom-0.5 -right-0.5 flex h-[10px] w-[10px] items-center justify-center rounded-[3px] border",
            isActive
              ? "border-border bg-background text-foreground"
              : "border-background bg-muted text-muted-foreground",
          )}
          aria-hidden="true"
        >
          <BadgeIcon
            className="h-[7px] w-[7px]"
            strokeWidth={2.5}
            style={
              badgeConfig?.offsetY
                ? { transform: `translateY(${badgeConfig.offsetY}px)` }
                : undefined
            }
          />
        </span>
      )}
    </span>
  );
}

interface SidebarProps {
  activeApp: AppId;
  /** 已折叠 claude-desktop → claude，用于功能可见性判定。 */
  featureApp: AppId;
  currentView: NavViewId | null;
  visibleApps?: VisibleApps;
  /** 选功能：若当前应用不支持，由调用方顺带切到该功能的首个可用应用。 */
  onSelectView: (view: NavViewId) => void;
  /** 在某个功能下选应用。 */
  onSelectApp: (app: AppId, view: NavViewId) => void;
  onOpenSettings: () => void;
  settingsActive: boolean;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** 面板有未完成操作时禁止导航。 */
  disabled?: boolean;
}

/**
 * 功能为一级、应用为二级的侧边栏。
 * 每个功能只列自己支持的应用，global 功能（统一供应商 / 智能体）不展开子项。
 */
export function Sidebar({
  activeApp,
  featureApp,
  currentView,
  visibleApps,
  onSelectView,
  onSelectApp,
  onOpenSettings,
  settingsActive,
  collapsed,
  onToggleCollapsed,
  disabled = false,
}: SidebarProps) {
  const { t } = useTranslation();
  const noDrag = { WebkitAppRegion: "no-drag" } as CSSProperties;
  // 只列当前应用支持的功能，避免 Codex 下出现 OpenClaw 专属项。
  // 跨应用切换靠功能展开后的应用子项完成。
  const visibleItems = NAV_ITEMS.filter(
    (item) => !item.apps || item.apps.includes(featureApp),
  );

  return (
    <TooltipProvider delayDuration={300}>
      <nav
        aria-label={t("nav.features")}
        className={cn(
          "flex h-full shrink-0 flex-col border-r border-border bg-muted/30",
          // 必须显式给 flex-basis：作为 flex row 子项时 basis:auto 不会回退到 width，
          // 宽度会被内容的 min-content 接管，折叠/展开都不生效
          collapsed ? "w-14 basis-14" : "w-[212px] basis-[212px]",
        )}
        style={noDrag}
      >
        <div
          className={cn(
            "flex h-12 min-w-0 shrink-0 items-center border-b border-border",
            collapsed ? "justify-center" : "justify-between px-3",
          )}
        >
          {!collapsed && (
            <span className="truncate text-sm font-semibold">
              {t("app.title")}
            </span>
          )}
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={onToggleCollapsed}
                aria-label={t(collapsed ? "nav.expand" : "nav.collapse")}
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-background/60 hover:text-foreground"
              >
                {collapsed ? (
                  <PanelLeftOpen className="h-4 w-4" />
                ) : (
                  <PanelLeftClose className="h-4 w-4" />
                )}
              </button>
            </TooltipTrigger>
            <TooltipContent side="right">
              {t(collapsed ? "nav.expand" : "nav.collapse")}
            </TooltipContent>
          </Tooltip>
        </div>

        <div
          className={cn(
            "flex min-h-0 min-w-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2",
            collapsed && "items-center",
          )}
        >
          {visibleItems.map(({ id, labelKey, icon: Icon, scope }) => {
            const isCurrent = currentView === id;
            const label = t(labelKey);
            const childApps = getAppsForView(id, visibleApps ?? {}, APP_IDS);
            const expanded = isCurrent && !collapsed && childApps.length > 0;

            const row = (
              <button
                type="button"
                onClick={() => onSelectView(id)}
                disabled={disabled && !isCurrent}
                aria-current={isCurrent ? "page" : undefined}
                className={cn(
                  "flex h-9 shrink-0 items-center rounded-lg text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-40",
                  // 不能用 w-full：它相对 nav 自身宽度求值，会把 nav 的 flex
                  // 自动最小尺寸锁在当前宽度，折叠/展开两个方向都失效。
                  // 展开态靠 flex column 的默认 stretch 自然铺满。
                  collapsed ? "w-10 justify-center px-0" : "gap-2.5 px-2.5",
                  isCurrent
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:bg-background/60 hover:text-foreground",
                )}
              >
                <Icon className="h-4 w-4 shrink-0" />
                {!collapsed && (
                  <>
                    <span className="min-w-0 flex-1 truncate text-left">
                      {label}
                    </span>
                    {scope !== "global" && childApps.length > 0 && (
                      <ChevronRight
                        className={cn(
                          "h-3.5 w-3.5 shrink-0 transition-transform",
                          expanded && "rotate-90",
                        )}
                        aria-hidden="true"
                      />
                    )}
                  </>
                )}
              </button>
            );

            return (
              <div key={id} className="flex shrink-0 flex-col">
                {collapsed ? (
                  <Tooltip>
                    <TooltipTrigger asChild>{row}</TooltipTrigger>
                    <TooltipContent side="right">{label}</TooltipContent>
                  </Tooltip>
                ) : (
                  row
                )}

                {expanded && (
                  <div className="mt-0.5 flex flex-col gap-0.5 border-l border-border pb-1 pl-3 ml-4">
                    {childApps.map((app) => {
                      const appActive = app === activeApp;
                      return (
                        <button
                          key={app}
                          type="button"
                          onClick={() => onSelectApp(app, id)}
                          disabled={disabled}
                          aria-current={appActive ? "true" : undefined}
                          className={cn(
                            "flex h-8 shrink-0 items-center gap-2 rounded-md px-2 text-[13px] transition-colors disabled:pointer-events-none disabled:opacity-40",
                            appActive
                              ? "bg-background font-medium text-foreground shadow-sm"
                              : "text-muted-foreground hover:bg-background/60 hover:text-foreground",
                          )}
                        >
                          <AppGlyph app={app} isActive={appActive} />
                          <span className="min-w-0 flex-1 truncate text-left">
                            {getAppLabel(app)}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>

        <div
          className={cn(
            "flex min-w-0 shrink-0 items-center border-t border-border p-2",
            collapsed && "justify-center",
          )}
        >
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={onOpenSettings}
                aria-label={t("nav.settings")}
                aria-current={settingsActive ? "true" : undefined}
                className={cn(
                  "flex h-9 items-center rounded-lg text-sm font-medium transition-colors",
                  collapsed ? "w-10 justify-center" : "flex-1 gap-2.5 px-2.5",
                  settingsActive
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:bg-background/60 hover:text-foreground",
                )}
              >
                <Settings className="h-4 w-4 shrink-0" />
                {!collapsed && <span>{t("nav.settings")}</span>}
              </button>
            </TooltipTrigger>
            <TooltipContent side="right">{t("nav.settings")}</TooltipContent>
          </Tooltip>
        </div>
      </nav>
    </TooltipProvider>
  );
}
