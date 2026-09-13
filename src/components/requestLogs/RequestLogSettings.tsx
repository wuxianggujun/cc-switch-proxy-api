import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { useSaveRequestTraceConfig } from "@/lib/query/requestTraces";
import type { RequestTraceConfig } from "@/types/requestTrace";

export function RequestLogSettings({
  config,
  onClose,
}: {
  config: RequestTraceConfig;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(config);
  const save = useSaveRequestTraceConfig();
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !save.isPending) onClose();
      }}
    >
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("requestLogs.settings")}</DialogTitle>
          <DialogDescription>{t("requestLogs.configHint")}</DialogDescription>
        </DialogHeader>
        <form
          className="space-y-5 px-6 py-4"
          onSubmit={async (event) => {
            event.preventDefault();
            try {
              await save.mutateAsync(draft);
              toast.success(t("requestLogs.saveDone"));
              onClose();
            } catch (error) {
              toast.error(String(error));
            }
          }}
        >
          <div className="space-y-3">
            <label className="flex items-center justify-between text-sm">
              {t("requestLogs.enabled")}
              <Switch
                checked={draft.enabled}
                onCheckedChange={(enabled) => setDraft({ ...draft, enabled })}
              />
            </label>
            <label className="flex items-center justify-between text-sm">
              {t("requestLogs.captureBodies")}
              <Switch
                checked={draft.captureBodies}
                onCheckedChange={(captureBodies) =>
                  setDraft({ ...draft, captureBodies })
                }
              />
            </label>
          </div>
          <div className="grid grid-cols-2 gap-4">
            <label className="space-y-2 text-sm">
              {t("requestLogs.retention")}
              <Input
                type="number"
                min={1}
                max={90}
                required
                value={draft.retentionDays}
                onChange={(e) =>
                  setDraft({ ...draft, retentionDays: Number(e.target.value) })
                }
              />
            </label>
            <label className="space-y-2 text-sm">
              {t("requestLogs.maxEntries")}
              <Input
                type="number"
                min={10}
                max={100000}
                required
                value={draft.maxEntries}
                onChange={(e) =>
                  setDraft({ ...draft, maxEntries: Number(e.target.value) })
                }
              />
            </label>
            <label className="space-y-2 text-sm">
              {t("requestLogs.maxBody")}
              <Input
                type="number"
                min={4}
                max={4096}
                required
                value={draft.maxBodyBytes / 1024}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    maxBodyBytes: Number(e.target.value) * 1024,
                  })
                }
              />
            </label>
            <label className="space-y-2 text-sm">
              {t("requestLogs.maxStorage")}
              <Input
                type="number"
                min={32}
                max={2048}
                required
                value={draft.maxStorageMb}
                onChange={(e) =>
                  setDraft({ ...draft, maxStorageMb: Number(e.target.value) })
                }
              />
            </label>
          </div>
          <p className="text-xs leading-5 text-muted-foreground">
            {t("requestLogs.credentialsHint")}
          </p>
          <div className="flex justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={save.isPending}
              onClick={onClose}
            >
              {t("requestLogs.cancel")}
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {t("requestLogs.save")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
