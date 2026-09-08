import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { checkinApi } from "@/lib/api/checkin";
import type { CheckinSite } from "@/types/checkin";
import { extractErrorMessage } from "@/utils/errorUtils";

export const checkinKeys = {
  all: ["checkin"] as const,
  config: ["checkin", "config"] as const,
};

export function useCheckinConfig() {
  return useQuery({
    queryKey: checkinKeys.config,
    queryFn: () => checkinApi.getConfig(),
    // Scheduling lives in Rust; refresh results while the panel is open without
    // relying on the page itself to keep the daily task alive.
    refetchInterval: 30_000,
  });
}

export function useUpsertCheckinSite() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (site: CheckinSite) => checkinApi.upsertSite(site),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
      toast.success(t("checkin.saveSuccess"));
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.saveError"));
    },
  });
}

export function useDeleteCheckinSite() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (id: string) => checkinApi.deleteSite(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
      toast.success(t("checkin.deleteSuccess"));
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.deleteError"));
    },
  });
}

export function useSetCheckinSchedule() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: ({
      scheduleEnabled,
      scheduleHour,
    }: {
      scheduleEnabled: boolean;
      scheduleHour: number;
    }) => checkinApi.setSchedule(scheduleEnabled, scheduleHour),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.scheduleError"));
    },
  });
}

/**
 * 单站签到。成功与失败都要落 toast：签到「失败」是正常业务结果
 * （例如今日已签到），不是异常，所以用 warning 而非 error。
 */
export function useRunCheckinSite() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (id: string) => checkinApi.runSite(id),
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
      if (result.status === "success") {
        toast.success(result.message || t("checkin.runSuccess"));
      } else if (result.status === "blocked") {
        // 被 CF 拦截不是业务失败，提示语要指向「重新验证」而非「今天已签过」。
        toast.error(result.message || t("checkin.runBlocked"));
      } else {
        toast.warning(result.message || t("checkin.runFailed"));
      }
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.runError"));
    },
  });
}

/**
 * 主动重新过 Cloudflare 验证。会弹出验证窗口，非交互式挑战自动完成，
 * 交互式挑战需用户点一下，所以耗时可能较长。
 */
export function useRefreshCheckinClearance() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (id: string) => checkinApi.refreshClearance(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
      toast.success(t("checkin.clearanceRefreshed"));
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.clearanceError"));
    },
  });
}

export function useRunAllCheckinSites() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: () => checkinApi.runAll(),
    onSuccess: (results) => {
      void queryClient.invalidateQueries({ queryKey: checkinKeys.config });
      const succeeded = results.filter(
        ([, result]) => result.status === "success",
      ).length;
      toast.success(
        t("checkin.runAllDone", {
          succeeded,
          total: results.length,
        }),
      );
    },
    onError: (error) => {
      toast.error(extractErrorMessage(error) || t("checkin.runError"));
    },
  });
}
