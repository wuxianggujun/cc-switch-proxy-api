import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Plus, ShieldCheck, Trash2 } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { deepClone } from "@/utils/deepClone";
import { useRefreshCheckinClearance } from "@/hooks/useCheckin";
import { CheckinBrowserAccountControls } from "./CheckinBrowserAccountControls";
import {
  emptyCheckinBrowser,
  emptyCheckinLogin,
  emptyCheckinSite,
  isClearanceFresh,
  type CheckinAuthKind,
  type CheckinBodyKind,
  type CheckinBrowser,
  type CheckinLogin,
  type CheckinSite,
} from "@/types/checkin";

const METHODS = ["POST", "GET", "PUT", "PATCH"] as const;
const BODY_KINDS: CheckinBodyKind[] = ["none", "json", "form"];

interface CheckinSiteFormModalProps {
  open: boolean;
  /** null 表示新建。 */
  site: CheckinSite | null;
  pending?: boolean;
  onSave: (site: CheckinSite) => void;
  onCancel: () => void;
}

export function CheckinSiteFormModal({
  open,
  site,
  pending = false,
  onSave,
  onCancel,
}: CheckinSiteFormModalProps) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<CheckinSite>(emptyCheckinSite);
  const [login, setLogin] = useState<CheckinLogin>(emptyCheckinLogin);
  const [browser, setBrowser] = useState<CheckinBrowser>(emptyCheckinBrowser);
  const refreshClearance = useRefreshCheckinClearance();

  // 每次打开都从 props 重建，避免上一次编辑的残留串到下一个站点。
  useEffect(() => {
    if (!open) return;
    setDraft(site ? deepClone(site) : emptyCheckinSite());
    setLogin(site?.login ? deepClone(site.login) : emptyCheckinLogin());
    setBrowser(site?.browser ? deepClone(site.browser) : emptyCheckinBrowser());
  }, [open, site]);

  const patch = (updates: Partial<CheckinSite>) =>
    setDraft((prev) => ({ ...prev, ...updates }));

  const patchRequest = (updates: Partial<CheckinSite["request"]>) =>
    setDraft((prev) => ({ ...prev, request: { ...prev.request, ...updates } }));

  const setHeader = (index: number, key: "name" | "value", value: string) =>
    setDraft((prev) => {
      const headers = [...prev.request.headers];
      headers[index] = { ...headers[index], [key]: value };
      return { ...prev, request: { ...prev.request, headers } };
    });

  const addHeader = () =>
    patchRequest({
      headers: [...draft.request.headers, { name: "", value: "" }],
    });

  const removeHeader = (index: number) =>
    patchRequest({
      headers: draft.request.headers.filter((_, i) => i !== index),
    });

  const handleSave = () => {
    // 只落当前认证方式需要的字段：authKind 非 login 时丢掉 login，
    // 避免密码残留在配置里。browser 提交时不带 cached，后端会保留旧凭证。
    onSave({
      ...draft,
      login: draft.authKind === "login" ? login : undefined,
      browser:
        draft.authKind === "browser"
          ? {
              challengeUrl: browser.challengeUrl,
              loginUrl: browser.loginUrl ?? "",
            }
          : undefined,
    });
  };

  // 已存盘的站点才有凭证可刷新：过闸需要后端按 id 读配置。
  const canRefresh = site !== null && draft.authKind === "browser";
  const browserActionsReady =
    open &&
    canRefresh &&
    Boolean(site?.id.trim()) &&
    site?.authKind === "browser" &&
    draft.siteUrl.trim() === site.siteUrl.trim() &&
    draft.request.url.trim() === site.request.url.trim() &&
    browser.challengeUrl.trim() === (site.browser?.challengeUrl ?? "").trim() &&
    (browser.loginUrl ?? "").trim() === (site.browser?.loginUrl ?? "").trim();
  const cached = site?.browser?.cached;
  const clearanceHint = !cached
    ? t("checkin.form.clearanceNone")
    : isClearanceFresh(cached)
      ? t("checkin.form.clearanceValid")
      : t("checkin.form.clearanceExpired");

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-2xl max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {site ? t("checkin.form.editTitle") : t("checkin.form.addTitle")}
          </DialogTitle>
          <DialogDescription>{t("checkin.form.description")}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4 px-6 py-4">
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="checkin-name">{t("checkin.form.name")}</Label>
              <Input
                id="checkin-name"
                value={draft.name}
                onChange={(e) => patch({ name: e.target.value })}
                placeholder={t("checkin.form.namePlaceholder")}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="checkin-site-url">
                {t("checkin.form.siteUrl")}
              </Label>
              <Input
                id="checkin-site-url"
                value={draft.siteUrl}
                onChange={(e) => patch({ siteUrl: e.target.value })}
                placeholder="https://example.com"
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <Label>{t("checkin.form.authKind")}</Label>
            <Select
              value={draft.authKind}
              onValueChange={(value) =>
                patch({ authKind: value as CheckinAuthKind })
              }
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="header">
                  {t("checkin.form.authHeader")}
                </SelectItem>
                <SelectItem value="login">
                  {t("checkin.form.authLogin")}
                </SelectItem>
                <SelectItem value="browser">
                  {t("checkin.form.authBrowser")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          {draft.authKind === "browser" && (
            <fieldset className="space-y-3 rounded-lg border border-border p-3">
              <legend className="px-1 text-xs font-medium text-muted-foreground">
                {t("checkin.form.browserSection")}
              </legend>

              <p className="text-xs text-muted-foreground">
                {t("checkin.form.browserHint")}
              </p>

              <div className="space-y-1.5">
                <Label htmlFor="checkin-browser-login-url">
                  {t("checkin.form.browserLoginUrl")}
                </Label>
                <Input
                  id="checkin-browser-login-url"
                  value={browser.loginUrl ?? ""}
                  onChange={(e) =>
                    setBrowser({ ...browser, loginUrl: e.target.value })
                  }
                  placeholder={draft.siteUrl || "https://example.com/login"}
                />
                <p className="text-xs text-muted-foreground">
                  {t("checkin.form.browserLoginUrlHint")}
                </p>
              </div>

              <CheckinBrowserAccountControls
                siteId={site?.id}
                disabled={
                  !browserActionsReady || pending || refreshClearance.isPending
                }
                needsLogin={site?.lastResult?.needsLogin}
              />

              <div className="space-y-1.5">
                <Label htmlFor="checkin-challenge-url">
                  {t("checkin.form.challengeUrl")}
                </Label>
                <Input
                  id="checkin-challenge-url"
                  value={browser.challengeUrl}
                  onChange={(e) =>
                    setBrowser({ ...browser, challengeUrl: e.target.value })
                  }
                  placeholder={draft.siteUrl || "https://example.com"}
                />
                <p className="text-xs text-muted-foreground">
                  {t("checkin.form.challengeUrlHint")}
                </p>
              </div>

              {canRefresh && (
                <div className="flex items-center justify-between gap-3">
                  <span className="text-xs text-muted-foreground">
                    {clearanceHint}
                  </span>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => refreshClearance.mutate(site.id)}
                    disabled={
                      !browserActionsReady ||
                      pending ||
                      refreshClearance.isPending
                    }
                  >
                    {refreshClearance.isPending ? (
                      <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <ShieldCheck className="mr-1 h-3.5 w-3.5" />
                    )}
                    {t("checkin.form.refreshClearance")}
                  </Button>
                </div>
              )}

              <p className="text-xs text-amber-600 dark:text-amber-400">
                {t("checkin.form.proxyPoolWarning")}
              </p>
            </fieldset>
          )}

          {draft.authKind === "login" && (
            <fieldset className="space-y-3 rounded-lg border border-border p-3">
              <legend className="px-1 text-xs font-medium text-muted-foreground">
                {t("checkin.form.loginSection")}
              </legend>

              <div className="space-y-1.5">
                <Label htmlFor="checkin-login-url">
                  {t("checkin.form.loginUrl")}
                </Label>
                <Input
                  id="checkin-login-url"
                  value={login.url}
                  onChange={(e) => setLogin({ ...login, url: e.target.value })}
                  placeholder="https://example.com/api/user/login"
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-1.5">
                  <Label htmlFor="checkin-username">
                    {t("checkin.form.username")}
                  </Label>
                  <Input
                    id="checkin-username"
                    value={login.username}
                    onChange={(e) =>
                      setLogin({ ...login, username: e.target.value })
                    }
                    autoComplete="off"
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="checkin-password">
                    {t("checkin.form.password")}
                  </Label>
                  <Input
                    id="checkin-password"
                    type="password"
                    value={login.password}
                    onChange={(e) =>
                      setLogin({ ...login, password: e.target.value })
                    }
                    autoComplete="off"
                  />
                </div>
              </div>

              <div className="grid grid-cols-3 gap-3">
                <div className="space-y-1.5">
                  <Label htmlFor="checkin-username-field">
                    {t("checkin.form.usernameField")}
                  </Label>
                  <Input
                    id="checkin-username-field"
                    value={login.usernameField}
                    onChange={(e) =>
                      setLogin({ ...login, usernameField: e.target.value })
                    }
                    placeholder="username"
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="checkin-password-field">
                    {t("checkin.form.passwordField")}
                  </Label>
                  <Input
                    id="checkin-password-field"
                    value={login.passwordField}
                    onChange={(e) =>
                      setLogin({ ...login, passwordField: e.target.value })
                    }
                    placeholder="password"
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>{t("checkin.form.loginBodyKind")}</Label>
                  <Select
                    value={login.bodyKind}
                    onValueChange={(value) =>
                      setLogin({ ...login, bodyKind: value as CheckinBodyKind })
                    }
                  >
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="json">JSON</SelectItem>
                      <SelectItem value="form">Form</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              </div>

              <p className="text-xs text-amber-600 dark:text-amber-400">
                {t("checkin.form.plaintextWarning")}
              </p>
            </fieldset>
          )}

          <fieldset className="space-y-3 rounded-lg border border-border p-3">
            <legend className="px-1 text-xs font-medium text-muted-foreground">
              {t("checkin.form.requestSection")}
            </legend>

            <div className="flex gap-3">
              <div className="w-28 shrink-0 space-y-1.5">
                <Label>{t("checkin.form.method")}</Label>
                <Select
                  value={draft.request.method}
                  onValueChange={(value) => patchRequest({ method: value })}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {METHODS.map((method) => (
                      <SelectItem key={method} value={method}>
                        {method}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="min-w-0 flex-1 space-y-1.5">
                <Label htmlFor="checkin-url">{t("checkin.form.url")}</Label>
                <Input
                  id="checkin-url"
                  value={draft.request.url}
                  onChange={(e) => patchRequest({ url: e.target.value })}
                  placeholder="https://example.com/api/user/check_in"
                />
              </div>
            </div>

            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <Label>{t("checkin.form.headers")}</Label>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={addHeader}
                >
                  <Plus className="mr-1 h-3.5 w-3.5" />
                  {t("common.add")}
                </Button>
              </div>
              {draft.request.headers.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  {draft.authKind === "header"
                    ? t("checkin.form.headersHintRequired")
                    : t("checkin.form.headersHintOptional")}
                </p>
              ) : (
                <div className="space-y-2">
                  {draft.request.headers.map((header, index) => (
                    <div key={index} className="flex gap-2">
                      <Input
                        value={header.name}
                        onChange={(e) =>
                          setHeader(index, "name", e.target.value)
                        }
                        placeholder="Cookie"
                        className="w-1/3"
                      />
                      <Input
                        value={header.value}
                        onChange={(e) =>
                          setHeader(index, "value", e.target.value)
                        }
                        placeholder="session=..."
                        className="min-w-0 flex-1"
                      />
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        onClick={() => removeHeader(index)}
                        aria-label={t("common.delete")}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  ))}
                </div>
              )}
            </div>

            <div className="grid grid-cols-3 gap-3">
              <div className="space-y-1.5">
                <Label>{t("checkin.form.bodyKind")}</Label>
                <Select
                  value={draft.request.bodyKind}
                  onValueChange={(value) =>
                    patchRequest({ bodyKind: value as CheckinBodyKind })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {BODY_KINDS.map((kind) => (
                      <SelectItem key={kind} value={kind}>
                        {t(`checkin.form.bodyKind_${kind}`)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="col-span-2 space-y-1.5">
                <Label htmlFor="checkin-success">
                  {t("checkin.form.successContains")}
                </Label>
                <Input
                  id="checkin-success"
                  value={draft.request.successContains}
                  onChange={(e) =>
                    patchRequest({ successContains: e.target.value })
                  }
                  placeholder={t("checkin.form.successPlaceholder")}
                />
              </div>
            </div>
            <p className="text-xs text-muted-foreground">
              {t("checkin.form.successHint")}
            </p>

            {draft.request.bodyKind !== "none" && (
              <div className="space-y-1.5">
                <Label htmlFor="checkin-body">{t("checkin.form.body")}</Label>
                <Textarea
                  id="checkin-body"
                  value={draft.request.body}
                  onChange={(e) => patchRequest({ body: e.target.value })}
                  rows={3}
                  className="font-mono text-xs"
                  placeholder={
                    draft.request.bodyKind === "json" ? "{}" : "key=value&a=1"
                  }
                />
              </div>
            )}
          </fieldset>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button onClick={handleSave} disabled={pending}>
            {pending ? t("common.saving") : t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
