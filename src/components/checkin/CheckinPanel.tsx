import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CalendarCheck,
  ExternalLink,
  Pencil,
  Play,
  Plus,
  Trash2,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CheckinStatusBadge } from "./CheckinStatusBadge";
import { CheckinSiteFormModal } from "./CheckinSiteFormModal";
import {
  useCheckinConfig,
  useDeleteCheckinSite,
  useRunAllCheckinSites,
  useRunCheckinSite,
  useSetCheckinSchedule,
  useUpsertCheckinSite,
} from "@/hooks/useCheckin";
import type { CheckinSite } from "@/types/checkin";

const HOURS = Array.from({ length: 24 }, (_, i) => i);

function formatTimestamp(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString();
}

export function CheckinPanel() {
  const { t } = useTranslation();
  const { data: config, isLoading } = useCheckinConfig();

  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<CheckinSite | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<CheckinSite | null>(null);

  const upsert = useUpsertCheckinSite();
  const remove = useDeleteCheckinSite();
  const setSchedule = useSetCheckinSchedule();
  const runSite = useRunCheckinSite();
  const runAll = useRunAllCheckinSites();

  const sites = useMemo(
    () => [...(config?.sites ?? [])].sort((a, b) => a.sortIndex - b.sortIndex),
    [config?.sites],
  );

  const enabledCount = sites.filter((site) => site.enabled).length;

  const handleSave = (site: CheckinSite) => {
    upsert.mutate(site, {
      onSuccess: () => {
        setFormOpen(false);
        setEditing(null);
      },
    });
  };

  const handleToggleEnabled = (site: CheckinSite, enabled: boolean) => {
    upsert.mutate({ ...site, enabled });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col px-6">
      <div className="flex flex-wrap items-center justify-between gap-3 pb-3">
        <div className="flex items-center gap-3">
          <Button
            onClick={() => {
              setEditing(null);
              setFormOpen(true);
            }}
            size="sm"
          >
            <Plus className="mr-1 h-4 w-4" />
            {t("checkin.addSite")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => runAll.mutate()}
            disabled={runAll.isPending || enabledCount === 0}
          >
            <Play className="mr-1 h-3.5 w-3.5" />
            {runAll.isPending
              ? t("checkin.runningAll")
              : t("checkin.runAll", { count: enabledCount })}
          </Button>
        </div>

        <div className="flex items-center gap-2">
          <Switch
            checked={config?.scheduleEnabled ?? false}
            disabled={setSchedule.isPending}
            onCheckedChange={(checked) =>
              setSchedule.mutate({
                scheduleEnabled: checked,
                scheduleHour: config?.scheduleHour ?? 9,
              })
            }
          />
          <span className="text-sm text-muted-foreground">
            {t("checkin.scheduleLabel")}
          </span>
          <Select
            value={String(config?.scheduleHour ?? 9)}
            onValueChange={(value) =>
              setSchedule.mutate({
                scheduleEnabled: config?.scheduleEnabled ?? false,
                scheduleHour: Number(value),
              })
            }
          >
            <SelectTrigger className="w-24">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {HOURS.map((hour) => (
                <SelectItem key={hour} value={String(hour)}>
                  {String(hour).padStart(2, "0")}:00
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <p className="pb-3 text-xs text-amber-600 dark:text-amber-400">
        {t("checkin.plaintextNotice")}
      </p>

      {isLoading ? (
        <div className="flex flex-1 items-center justify-center text-muted-foreground">
          {t("common.loading")}
        </div>
      ) : sites.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center">
          <CalendarCheck className="h-12 w-12 text-muted-foreground/50" />
          <p className="text-muted-foreground">{t("checkin.empty")}</p>
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto pb-8">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-12" />
                <TableHead>{t("checkin.table.name")}</TableHead>
                <TableHead>{t("checkin.table.status")}</TableHead>
                <TableHead>{t("checkin.table.lastRun")}</TableHead>
                <TableHead>{t("checkin.table.message")}</TableHead>
                <TableHead className="w-32 text-right">
                  {t("checkin.table.actions")}
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {sites.map((site) => (
                <TableRow key={site.id}>
                  <TableCell>
                    <Switch
                      checked={site.enabled}
                      onCheckedChange={(checked) =>
                        handleToggleEnabled(site, checked)
                      }
                      aria-label={t("checkin.table.enabled")}
                    />
                  </TableCell>
                  <TableCell className="font-medium">
                    <span className="flex items-center gap-1.5">
                      {site.name}
                      {site.siteUrl && (
                        <a
                          href={site.siteUrl}
                          target="_blank"
                          rel="noreferrer noopener"
                          className="text-muted-foreground hover:text-foreground"
                          aria-label={t("checkin.table.openSite")}
                        >
                          <ExternalLink className="h-3 w-3" />
                        </a>
                      )}
                    </span>
                  </TableCell>
                  <TableCell>
                    <CheckinStatusBadge result={site.lastResult} />
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {site.lastResult
                      ? formatTimestamp(site.lastResult.at)
                      : "—"}
                  </TableCell>
                  <TableCell className="max-w-[240px] truncate text-sm text-muted-foreground">
                    {site.lastResult?.message || "—"}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex items-center justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => runSite.mutate(site.id)}
                        disabled={runSite.isPending || runAll.isPending}
                        aria-label={t("checkin.table.run")}
                      >
                        <Play className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => {
                          setEditing(site);
                          setFormOpen(true);
                        }}
                        aria-label={t("common.edit")}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => setDeleteTarget(site)}
                        aria-label={t("common.delete")}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      <CheckinSiteFormModal
        open={formOpen}
        site={editing}
        pending={upsert.isPending}
        onSave={handleSave}
        onCancel={() => {
          setFormOpen(false);
          setEditing(null);
        }}
      />

      <ConfirmDialog
        isOpen={deleteTarget !== null}
        title={t("checkin.deleteTitle")}
        message={t("checkin.deleteMessage", { name: deleteTarget?.name ?? "" })}
        variant="destructive"
        pending={remove.isPending}
        onConfirm={() => {
          if (deleteTarget) {
            remove.mutate(deleteTarget.id, {
              onSuccess: () => setDeleteTarget(null),
            });
          }
        }}
        onCancel={() => setDeleteTarget(null)}
      />
    </div>
  );
}
