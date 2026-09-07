import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useSettingsQuery } from "@/lib/query/queries";
import { useSaveSettingsMutation } from "@/lib/query/mutations";
import { UsageDashboard } from "@/components/usage/UsageDashboard";
import { ProxyStatusBar } from "./ProxyStatusBar";
import type { Settings } from "@/types";

/**
 * 首页：代理运行态 + 全量请求记录。
 *
 * 用量面板整体复用 UsageDashboard（原挂在设置页的 usage tab）。它的两个偏好项
 * 需要调用方提供读写通道，这里直连 settings query，避免依赖设置弹窗的表单层。
 */
export function HomePage() {
  const { data: settings } = useSettingsQuery();
  const saveMutation = useSaveSettingsMutation();
  const queryClient = useQueryClient();

  // 两个偏好项都是纯 UI 状态，后端无副作用，直接全量回存即可。
  // webdavSync / s3Sync 有各自独立的保存通道，透传会覆盖掉它们的最新值。
  const persist = useCallback(
    async (updates: Partial<Settings>): Promise<boolean> => {
      const current =
        queryClient.getQueryData<Settings>(["settings"]) ?? settings;
      if (!current) return false;
      const {
        webdavSync: _webdavSync,
        s3Sync: _s3Sync,
        ...rest
      } = { ...current, ...updates };
      try {
        await saveMutation.mutateAsync(rest as Settings);
        return true;
      } catch (error) {
        console.error("[HomePage] Failed to persist setting", error);
        return false;
      }
    },
    [queryClient, settings, saveMutation],
  );

  return (
    <div className="flex flex-col gap-4">
      <ProxyStatusBar />
      <UsageDashboard
        refreshIntervalMs={settings?.usageDashboardRefreshIntervalMs}
        onRefreshIntervalChange={(usageDashboardRefreshIntervalMs) =>
          persist({ usageDashboardRefreshIntervalMs })
        }
        sessionAutoSyncEnabled={settings?.sessionAutoSyncEnabled ?? true}
        onSessionAutoSyncEnabledChange={(sessionAutoSyncEnabled) =>
          persist({ sessionAutoSyncEnabled })
        }
      />
    </div>
  );
}
