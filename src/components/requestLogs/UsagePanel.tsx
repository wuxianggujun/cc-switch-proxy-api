import { UsageDashboard } from "@/components/usage/UsageDashboard";
import { useUsagePreferences } from "@/hooks/useUsagePreferences";

/**
 * 用量统计的挂载点。偏好项直连 settings query，不经过设置页的表单层。
 * 单独成组件是为了让 `useSettingsQuery` 只在用量模式下发起，切到追踪模式即卸载。
 */
export function UsagePanel() {
  const {
    refreshIntervalMs,
    sessionAutoSyncEnabled,
    setRefreshIntervalMs,
    setSessionAutoSyncEnabled,
  } = useUsagePreferences();

  return (
    <UsageDashboard
      refreshIntervalMs={refreshIntervalMs}
      onRefreshIntervalChange={setRefreshIntervalMs}
      sessionAutoSyncEnabled={sessionAutoSyncEnabled}
      onSessionAutoSyncEnabledChange={setSessionAutoSyncEnabled}
    />
  );
}
