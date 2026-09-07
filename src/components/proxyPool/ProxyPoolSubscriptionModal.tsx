/**
 * 新增代理池订阅
 *
 * remote 需要 URL，inline 需要粘贴正文；两者只校验各自必填项。
 * 后端也会校验，这里前置一遍是为了不让用户等一次 invoke 往返。
 */

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
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
import type {
  AddSubscriptionPayload,
  SubscriptionSource,
} from "@/lib/api/proxyPool";

/** 自动刷新间隔选项（秒）。0 表示不自动刷新。 */
const INTERVAL_OPTIONS = [0, 1800, 3600, 21600, 86400];

const DEFAULT_INTERVAL = 3600;

interface ProxyPoolSubscriptionModalProps {
  open: boolean;
  pending?: boolean;
  onSave: (payload: AddSubscriptionPayload) => void;
  onCancel: () => void;
}

export function ProxyPoolSubscriptionModal({
  open,
  pending = false,
  onSave,
  onCancel,
}: ProxyPoolSubscriptionModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [source, setSource] = useState<SubscriptionSource>("remote");
  const [url, setUrl] = useState("");
  const [content, setContent] = useState("");
  const [intervalSecs, setIntervalSecs] = useState(DEFAULT_INTERVAL);
  const [error, setError] = useState<string | null>(null);

  // 每次打开都重置，避免上一次的输入残留
  useEffect(() => {
    if (!open) return;
    setName("");
    setSource("remote");
    setUrl("");
    setContent("");
    setIntervalSecs(DEFAULT_INTERVAL);
    setError(null);
  }, [open]);

  const handleSubmit = () => {
    if (!name.trim()) {
      setError(t("proxyPool.error.nameRequired"));
      return;
    }
    if (source === "remote" && !url.trim()) {
      setError(t("proxyPool.error.urlRequired"));
      return;
    }
    if (source === "inline" && !content.trim()) {
      setError(t("proxyPool.error.contentRequired"));
      return;
    }
    setError(null);
    onSave({
      name: name.trim(),
      source,
      // 后端按 source 取值，另一路传空串即可
      url: source === "remote" ? url.trim() : "",
      content: source === "inline" ? content : "",
      updateIntervalSecs: intervalSecs,
    });
  };

  const intervalLabel = (secs: number) =>
    secs === 0
      ? t("proxyPool.form.intervalManual")
      : t("proxyPool.form.intervalHours", { hours: secs / 3600 });

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("proxyPool.form.addTitle")}</DialogTitle>
          <DialogDescription>
            {t("proxyPool.form.description")}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="pp-sub-name">{t("proxyPool.form.name")}</Label>
            <Input
              id="pp-sub-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("proxyPool.form.namePlaceholder")}
            />
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="pp-sub-source">{t("proxyPool.form.source")}</Label>
            <Select
              value={source}
              onValueChange={(value) => {
                setSource(value as SubscriptionSource);
                setError(null);
              }}
            >
              <SelectTrigger id="pp-sub-source">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="remote">
                  {t("proxyPool.source.remote")}
                </SelectItem>
                <SelectItem value="inline">
                  {t("proxyPool.source.inline")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          {source === "remote" ? (
            <div className="space-y-1.5">
              <Label htmlFor="pp-sub-url">{t("proxyPool.form.url")}</Label>
              <Input
                id="pp-sub-url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://example.com/subscribe"
                className="font-mono text-sm"
              />
            </div>
          ) : (
            <div className="space-y-1.5">
              <Label htmlFor="pp-sub-content">
                {t("proxyPool.form.content")}
              </Label>
              <Textarea
                id="pp-sub-content"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                rows={8}
                placeholder={
                  "socks5://user:pass@1.2.3.4:1080\nhttp://5.6.7.8:3128"
                }
                className="font-mono text-xs"
              />
              <p className="text-xs text-muted-foreground">
                {t("proxyPool.form.contentHint")}
              </p>
            </div>
          )}

          <div className="space-y-1.5">
            <Label htmlFor="pp-sub-interval">
              {t("proxyPool.form.interval")}
            </Label>
            <Select
              value={String(intervalSecs)}
              onValueChange={(value) => setIntervalSecs(Number(value))}
            >
              <SelectTrigger id="pp-sub-interval">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {INTERVAL_OPTIONS.map((secs) => (
                  <SelectItem key={secs} value={String(secs)}>
                    {intervalLabel(secs)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <p className="text-xs text-muted-foreground">
            {t("proxyPool.form.protocolNotice")}
          </p>

          {error && (
            <p className="text-sm text-destructive" role="alert">
              {error}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={pending}>
            {t("common.cancel")}
          </Button>
          <Button onClick={handleSubmit} disabled={pending}>
            {pending ? t("proxyPool.form.saving") : t("common.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
