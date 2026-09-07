import { useEffect, useState } from "react";
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
  onSubmit: (values: KeyFormValues) => void;
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
  const [name, setName] = useState("");
  const [internalPriority, setInternalPriority] = useState("50");

  useEffect(() => {
    if (!open) return;
    setApiKey("");
    setName("");
    setInternalPriority("50");
  }, [open]);

  const trimmedKey = apiKey.trim();
  const parsedPriority = Number.parseInt(internalPriority, 10);
  const canSubmit =
    trimmedKey.length > 0 && Number.isFinite(parsedPriority) && !pending;

  const submit = () => {
    if (!canSubmit) return;
    onSubmit({
      apiKey: trimmedKey,
      name: name.trim() || undefined,
      internalPriority: parsedPriority,
    });
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t("apiAccess.keys.add")}</DialogTitle>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="key-value">{t("apiAccess.form.apiKey")}</Label>
            <Input
              id="key-value"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={t("apiAccess.form.apiKeyPlaceholder")}
              className="font-mono text-sm"
              autoComplete="off"
              spellCheck={false}
            />
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
              value={internalPriority}
              onChange={(e) => setInternalPriority(e.target.value)}
            />
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel} disabled={pending}>
            {t("apiAccess.form.cancel")}
          </Button>
          <Button onClick={submit} disabled={!canSubmit}>
            {t("apiAccess.form.save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
