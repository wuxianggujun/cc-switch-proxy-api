import { Loader2, LogIn, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  useCheckinBrowserSession,
  useOpenCheckinLogin,
} from "@/hooks/useCheckin";

interface CheckinBrowserAccountControlsProps {
  siteId?: string;
  disabled?: boolean;
  needsLogin?: boolean;
}

export function CheckinBrowserAccountControls({
  siteId,
  disabled = false,
  needsLogin = false,
}: CheckinBrowserAccountControlsProps) {
  const { t } = useTranslation();
  const canOperate = Boolean(siteId?.trim()) && !disabled;
  const openLogin = useOpenCheckinLogin();
  const session = useCheckinBrowserSession(canOperate ? siteId : undefined);

  const statusHint = !canOperate
    ? t("checkin.form.browserSaveFirst")
    : session.isLoading
      ? t("checkin.form.browserSessionChecking")
      : session.isError
        ? t("checkin.form.browserSessionError")
        : session.data?.loginWindowOpen
          ? t("checkin.form.browserLoginInProgress")
          : session.data?.accountCookieCount
            ? t("checkin.form.browserSessionCookies", {
                count: session.data.accountCookieCount,
              })
            : t("checkin.form.browserSessionEmpty");

  return (
    <div className="space-y-2 border-t border-border pt-3">
      <p className="text-xs text-muted-foreground">
        {t("checkin.form.browserProfileHint")}
      </p>
      {needsLogin && (
        <p className="text-xs text-amber-600 dark:text-amber-400">
          {t("checkin.loginRequired")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!canOperate || openLogin.isPending}
          onClick={() => siteId && openLogin.mutate(siteId)}
        >
          {openLogin.isPending ? (
            <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
          ) : (
            <LogIn className="mr-1 h-3.5 w-3.5" />
          )}
          {t("checkin.form.openLogin")}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={!canOperate || session.isFetching || openLogin.isPending}
          onClick={() => void session.refetch()}
        >
          <RefreshCw className="mr-1 h-3.5 w-3.5" />
          {t("checkin.form.browserSessionRefresh")}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground" role="status">
        {statusHint}
      </p>
      <p className="text-xs text-muted-foreground">
        {t("checkin.form.browserManualAuthHint")}
      </p>
    </div>
  );
}
