import {
  Book,
  Bot,
  Boxes,
  Brain,
  CalendarCheck,
  Cpu,
  FolderOpen,
  FileText,
  History,
  KeyRound,
  Layers,
  LayoutDashboard,
  Network,
  Server,
  Shield,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import type { AppId } from "@/lib/api/types";

/** 侧边栏功能视图。settings / skillsDiscovery 不在其中：前者是全局入口，后者是 skills 的子页。 */
export type NavViewId =
  | "home"
  | "apiAccess"
  | "requestLogs"
  | "providers"
  | "skills"
  | "prompts"
  | "sessions"
  | "mcp"
  | "agents"
  | "universal"
  | "workspace"
  | "openclawEnv"
  | "openclawTools"
  | "openclawAgents"
  | "hermesMemory"
  | "checkin"
  | "proxyPool";

export interface NavItem {
  id: NavViewId;
  labelKey: string;
  icon: LucideIcon;
  /**
   * 该视图适用的应用；undefined 表示全部可见。
   * 判定基于 sharedFeatureApp（claude-desktop 已折叠为 claude）。
   */
  apps?: AppId[];
  /** global 表示与应用无关，侧边栏里不展开应用子项。 */
  scope?: "global";
  /**
   * 归入供应商页顶部标签栏的视图：它们都依赖「当前是哪个供应商应用」，
   * 拆在侧边栏里会让同一上下文的功能散落两处。
   * 侧边栏只保留 providers 作为该组入口，其余成员不再单独列行。
   */
  group?: "providers";
}

const CLI_APPS: AppId[] = [
  "claude",
  "codex",
  "gemini",
  "grokbuild",
  "opencode",
  "hermes",
  "pi",
];

export const NAV_ITEMS: NavItem[] = [
  {
    id: "home",
    labelKey: "nav.home",
    icon: LayoutDashboard,
    scope: "global",
  },
  // 网关自己的路由表：一个接入点可挂多个密钥，与具体 CLI 无关，故为 global。
  {
    id: "apiAccess",
    labelKey: "nav.apiAccess",
    icon: Boxes,
    scope: "global",
  },
  {
    id: "providers",
    labelKey: "nav.providers",
    icon: Server,
    group: "providers",
  },
  {
    id: "requestLogs",
    labelKey: "nav.requestLogs",
    icon: FileText,
    scope: "global",
  },
  {
    id: "skills",
    labelKey: "nav.skills",
    icon: Wrench,
    apps: CLI_APPS,
    group: "providers",
  },
  {
    id: "prompts",
    labelKey: "nav.prompts",
    icon: Book,
    apps: CLI_APPS,
    group: "providers",
  },
  {
    id: "sessions",
    labelKey: "nav.sessions",
    icon: History,
    apps: [...CLI_APPS, "openclaw"],
    group: "providers",
  },
  {
    id: "mcp",
    labelKey: "nav.mcp",
    icon: Layers,
    apps: ["claude", "codex", "gemini", "grokbuild", "opencode", "hermes"],
    group: "providers",
  },
  {
    id: "workspace",
    labelKey: "nav.workspace",
    icon: FolderOpen,
    apps: ["openclaw"],
  },
  {
    id: "openclawEnv",
    labelKey: "nav.openclawEnv",
    icon: KeyRound,
    apps: ["openclaw"],
  },
  {
    id: "openclawTools",
    labelKey: "nav.openclawTools",
    icon: Shield,
    apps: ["openclaw"],
  },
  {
    id: "openclawAgents",
    labelKey: "nav.openclawAgents",
    icon: Cpu,
    apps: ["openclaw"],
  },
  {
    id: "hermesMemory",
    labelKey: "nav.hermesMemory",
    icon: Brain,
    apps: ["hermes"],
  },
  // 统一供应商本身与具体 CLI 无关（scope: global），但它编辑的就是供应商，
  // 放在供应商页标签栏里比单列一行更贴近使用场景。
  {
    id: "universal",
    labelKey: "nav.universal",
    icon: Layers,
    scope: "global",
    group: "providers",
  },
  // 公益站账号与具体 CLI 无关（同一站常同时供 claude 和 codex），故为 global。
  {
    id: "checkin",
    labelKey: "nav.checkin",
    icon: CalendarCheck,
    scope: "global",
  },
  { id: "agents", labelKey: "nav.agents", icon: Bot, scope: "global" },
  // 代理池聚合出站节点，与具体 CLI 无关，故为 global。
  {
    id: "proxyPool",
    labelKey: "nav.proxyPool",
    icon: Network,
    scope: "global",
  },
];

/** 供应商页标签栏的顺序；不在表里的组成员按 NAV_ITEMS 原序排在后面。 */
const PROVIDER_TAB_ORDER: NavViewId[] = [
  "providers",
  "skills",
  "prompts",
  "sessions",
  "mcp",
  "universal",
];

export function isProviderTabView(view: NavViewId): boolean {
  return NAV_ITEMS.some(
    (item) => item.id === view && item.group === "providers",
  );
}

/** 当前应用下可见的供应商页标签（已按 apps 过滤）。 */
export function getProviderTabs(app: AppId): NavItem[] {
  const rank = (id: NavViewId) => {
    const index = PROVIDER_TAB_ORDER.indexOf(id);
    return index === -1 ? Number.POSITIVE_INFINITY : index;
  };
  return NAV_ITEMS.filter(
    (item) =>
      item.group === "providers" && (!item.apps || item.apps.includes(app)),
  ).sort((a, b) => rank(a.id) - rank(b.id));
}

export function isNavViewAvailable(view: NavViewId, app: AppId): boolean {
  const item = NAV_ITEMS.find((entry) => entry.id === view);
  if (!item) return false;
  return !item.apps || item.apps.includes(app);
}

/**
 * 该功能下可选的应用（已按 visibleApps 过滤）。
 * claude-desktop 折叠为 claude 判定，所以 claude 支持的功能它也一并出现。
 */
export function getAppsForView(
  view: NavViewId,
  visibleApps: Partial<Record<AppId, boolean>>,
  allApps: AppId[],
): AppId[] {
  const item = NAV_ITEMS.find((entry) => entry.id === view);
  if (!item || item.scope === "global") return [];
  return allApps.filter((app) => {
    if (visibleApps[app] === false) return false;
    const featureApp: AppId = app === "claude-desktop" ? "claude" : app;
    return !item.apps || item.apps.includes(featureApp);
  });
}
