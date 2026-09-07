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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  UPSTREAM_LABELS,
  UPSTREAM_ORDER,
  type ApiEndpoint,
  type UpstreamType,
} from "@/types/apiGateway";

export interface EndpointFormValues {
  name: string;
  upstreamType: UpstreamType;
  baseUrl: string;
  models: string[];
  priority: number;
  notes?: string;
}

interface EndpointFormModalProps {
  open: boolean;
  /** 传入即为编辑模式 */
  endpoint?: ApiEndpoint;
  defaultUpstreamType: UpstreamType;
  pending?: boolean;
  onSubmit: (values: EndpointFormValues) => void;
  onCancel: () => void;
}

export function EndpointFormModal({
  open,
  endpoint,
  defaultUpstreamType,
  pending = false,
  onSubmit,
  onCancel,
}: EndpointFormModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [upstreamType, setUpstreamType] =
    useState<UpstreamType>(defaultUpstreamType);
  const [baseUrl, setBaseUrl] = useState("");
  const [modelsText, setModelsText] = useState("");
  const [priority, setPriority] = useState("100");
  const [notes, setNotes] = useState("");

  // 每次打开都重置：编辑填入原值，新增回到默认（含当前选中的上游类型）
  useEffect(() => {
    if (!open) return;
    setName(endpoint?.name ?? "");
    setUpstreamType(endpoint?.upstreamType ?? defaultUpstreamType);
    setBaseUrl(endpoint?.baseUrl ?? "");
    setModelsText((endpoint?.models ?? []).join("\n"));
    setPriority(String(endpoint?.priority ?? 100));
    setNotes(endpoint?.notes ?? "");
  }, [open, endpoint, defaultUpstreamType]);

  const trimmedName = name.trim();
  const trimmedUrl = baseUrl.trim();
  const parsedPriority = Number.parseInt(priority, 10);
  const canSubmit =
    trimmedName.length > 0 &&
    trimmedUrl.length > 0 &&
    Number.isFinite(parsedPriority) &&
    !pending;

  const submit = () => {
    if (!canSubmit) return;
    onSubmit({
      name: trimmedName,
      upstreamType,
      baseUrl: trimmedUrl,
      models: modelsText
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean),
      priority: parsedPriority,
      notes: notes.trim() || undefined,
    });
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {endpoint
              ? t("apiAccess.form.editTitle")
              : t("apiAccess.form.addTitle")}
          </DialogTitle>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-name">{t("apiAccess.form.name")}</Label>
            <Input
              id="ep-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("apiAccess.form.namePlaceholder")}
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-upstream">
              {t("apiAccess.form.upstreamType")}
            </Label>
            <Select
              value={upstreamType}
              onValueChange={(next) => setUpstreamType(next as UpstreamType)}
            >
              <SelectTrigger id="ep-upstream">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {UPSTREAM_ORDER.map((type) => (
                  <SelectItem key={type} value={type}>
                    {UPSTREAM_LABELS[type]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-url">{t("apiAccess.form.baseUrl")}</Label>
            <Input
              id="ep-url"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://api.example.com"
              className="font-mono text-sm"
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-models">{t("apiAccess.form.models")}</Label>
            <textarea
              id="ep-models"
              value={modelsText}
              onChange={(e) => setModelsText(e.target.value)}
              placeholder={t("apiAccess.form.modelsPlaceholder")}
              rows={4}
              className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-priority">{t("apiAccess.form.priority")}</Label>
            <Input
              id="ep-priority"
              type="number"
              value={priority}
              onChange={(e) => setPriority(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              {t("apiAccess.form.priorityHint")}
            </p>
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-notes">{t("apiAccess.form.notes")}</Label>
            <Input
              id="ep-notes"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
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
