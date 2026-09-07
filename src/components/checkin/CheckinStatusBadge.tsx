import { useTranslation } from "react-i18next";
import {
  CheckCircle2,
  CircleSlash,
  AlertTriangle,
  ShieldAlert,
  Minus,
} from "lucide-react";
import { cn } from "@/lib/utils";
import type { CheckinResult } from "@/types/checkin";

/**
 * 四态区分是刻意的：failed 表示请求送达但站点说没签上（最常见是
 * 「今日已签到」），属正常结果，不该和网络错误一样标红；blocked 表示
 * 被 Cloudflare 挡在门外，需要用户重新验证而不是重试。
 */
const STYLES = {
  success: {
    icon: CheckCircle2,
    className: "text-emerald-600 dark:text-emerald-400",
  },
  failed: {
    icon: CircleSlash,
    className: "text-amber-600 dark:text-amber-400",
  },
  error: {
    icon: AlertTriangle,
    className: "text-red-600 dark:text-red-400",
  },
  blocked: {
    icon: ShieldAlert,
    className: "text-orange-600 dark:text-orange-400",
  },
} as const;

export function CheckinStatusBadge({ result }: { result?: CheckinResult }) {
  const { t } = useTranslation();

  if (!result) {
    return (
      <span className="inline-flex items-center gap-1.5 text-sm text-muted-foreground">
        <Minus className="h-3.5 w-3.5" />
        {t("checkin.status.never")}
      </span>
    );
  }

  const style = STYLES[result.status];
  const Icon = style.icon;

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 text-sm font-medium",
        style.className,
      )}
      title={result.message || undefined}
    >
      <Icon className="h-3.5 w-3.5 shrink-0" />
      {t(`checkin.status.${result.status}`)}
    </span>
  );
}
