import { useEffect, useRef, useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export interface KeyFormValues {
  apiKey: string;
  name?: string;
  internalPriority: number;
}

interface KeyFormModalProps {
  open: boolean;
  pending?: boolean;
  onSubmit: (values: KeyFormValues) => Promise<void> | void;
  onCancel: () => void;
}

export function KeyFormModal({
  open,
  pending = false,
  onSubmit,
  onCancel,
}: KeyFormModalProps) {
  const { t } = useTranslation();
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [name, setName] = useState("");
  const [internalPriority, setInternalPriority] = useState("50");
  const [submitted, setSubmitted] = useState(false);
  const submitLock = useRef(false);

  useEffect(() => {
    if (!open) {
      setApiKey("");
      setShowKey(false);
      setSubmitted(false);
      submitLock.current = false;
      return;
    }
    setApiKey("");
    setShowKey(false);
    setName("");
    setInternalPriority("50");
    setSubmitted(false);
    submitLock.current = false;
  }, [open]);

  const trimmedKey = apiKey.trim();
  const parsedPriority = Number(internalPriority);
  const priorityValid =
    internalPriority.trim().length > 0 && Number.isSafeInteger(parsedPriority);
  const canSubmit = trimmedKey.length > 0 && priorityValid && !pending;

  const submit = async () => {
    setSubmitted(true);
    if (!canSubmit || submitLock.current) return;
    submitLock.current = true;
    try {
      await onSubmit({
        apiKey: trimmedKey,
        name: name.trim() || undefined,
        internalPriority: parsedPriority,
      });
    } finally {
      submitLock.current = false;
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t("apiAccess.keys.add")}</DialogTitle>
        </DialogHeader>

        <div className="flex flex-col gap-4 px-6 py-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="key-value">{t("apiAccess.form.apiKey")}</Label>
            <div className="relative">
              <Input
                id="key-value"
                type={showKey ? "text" : "password"}
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={t("apiAccess.form.apiKeyPlaceholder")}
                className="pr-10 font-mono text-sm"
                autoComplete="new-password"
                spellCheck={false}
                aria-invalid={submitted && !trimmedKey}
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="absolute right-0 top-0 h-full w-10 text-muted-foreground"
                onClick={() => setShowKey((visible) => !visible)}
                aria-label={
                  showKey
                    ? t("apiAccess.form.hideApiKey")
                    : t("apiAccess.form.showApiKey")
                }
              >
                {showKey ? (
                  <EyeOff className="h-4 w-4" />
                ) : (
                  <Eye className="h-4 w-4" />
                )}
              </Button>
            </div>
            {submitted && !trimmedKey && (
              <p className="text-xs text-destructive">
                {t("apiAccess.form.apiKeyRequired")}
              </p>
            )}
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="key-name">{t("apiAccess.form.name")}</Label>
            <Input
              id="key-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="key-priority">
              {t("apiAccess.form.internalPriority")}
            </Label>
            <Input
              id="key-priority"
              type="number"
              step="1"
              value={internalPriority}
              onChange={(e) => setInternalPriority(e.target.value)}
              aria-invalid={submitted && !priorityValid}
            />
            {submitted && !priorityValid && (
              <p className="text-xs text-destructive">
                {t("apiAccess.form.priorityInteger")}
              </p>
            )}
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={pending}>
            {t("apiAccess.form.cancel")}
          </Button>
          <Button onClick={submit} disabled={pending}>
            {pending ? t("common.saving") : t("apiAccess.form.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
