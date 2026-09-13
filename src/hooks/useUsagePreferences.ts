import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useSettingsQuery } from "@/lib/query/queries";
import { useSaveSettingsMutation } from "@/lib/query/mutations";
import type { Settings } from "@/types";

/**
 * 用量面板的两个偏好项。面板本身不依赖设置页的表单层，任何挂载点都能直接读写。
 */
export function useUsagePreferences() {
  const { data: settings } = useSettingsQuery();
  const saveMutation = useSaveSettingsMutation();
  const queryClient = useQueryClient();

  const persist = useCallback(
    async (updates: Partial<Settings>): Promise<boolean> => {
      const current =
        queryClient.getQueryData<Settings>(["settings"]) ?? settings;
      if (!current) return false;
      // webdavSync / s3Sync 有各自独立的保存通道，透传会覆盖掉它们的最新值。
      const {
        webdavSync: _webdavSync,
        s3Sync: _s3Sync,
        ...rest
      } = { ...current, ...updates };
      try {
        await saveMutation.mutateAsync(rest as Settings);
        return true;
      } catch (error) {
        console.error("[useUsagePreferences] Failed to persist setting", error);
        return false;
      }
    },
    [queryClient, settings, saveMutation],
  );

  return {
    refreshIntervalMs: settings?.usageDashboardRefreshIntervalMs,
    sessionAutoSyncEnabled: settings?.sessionAutoSyncEnabled ?? true,
    setRefreshIntervalMs: (usageDashboardRefreshIntervalMs: number) =>
      persist({ usageDashboardRefreshIntervalMs }),
    setSessionAutoSyncEnabled: (sessionAutoSyncEnabled: boolean) =>
      persist({ sessionAutoSyncEnabled }),
  };
}
