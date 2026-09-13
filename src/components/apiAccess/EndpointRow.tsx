import { useTranslation } from "react-i18next";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import {
  GripVertical,
  Pencil,
  Trash2,
  KeyRound,
  RotateCcw,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { ApiEndpoint, ApiKey } from "@/types/apiGateway";

/** 密钥的不可用状态。硬状态优先于冷却展示——前者更严重。 */
function keyPenalty(key: ApiKey, nowSecs: number) {
  if (key.hardState) return key.hardState;
  if (key.cooldownUntil && key.cooldownUntil > nowSecs) return "cooldown";
  return null;
}

interface EndpointRowProps {
  endpoint: ApiEndpoint;
  keys: ApiKey[] | undefined;
  expanded: boolean;
  onToggleExpanded: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  onEdit: () => void;
  onDelete: () => void;
  onAddKey: () => void;
  onDeleteKey: (key: ApiKey) => void;
  onToggleKeyEnabled: (key: ApiKey, enabled: boolean) => void;
  onClearKeyPenalty: (key: ApiKey) => void;
}

export function EndpointRow({
  endpoint,
  keys,
  expanded,
  onToggleExpanded,
  onToggleEnabled,
  onEdit,
  onDelete,
  onAddKey,
  onDeleteKey,
  onToggleKeyEnabled,
  onClearKeyPenalty,
}: EndpointRowProps) {
  const { t } = useTranslation();
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: endpoint.id });
  const nowSecs = Math.floor(Date.now() / 1000);

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className={cn(
        "border-b border-border/50 last:border-b-0",
        isDragging && "relative z-10 bg-card shadow-lg",
      )}
    >
      <div className="flex items-start gap-3 px-4 py-4">
        <button
          type="button"
          className="mt-0.5 cursor-grab touch-none text-muted-foreground/50 hover:text-muted-foreground active:cursor-grabbing"
          aria-label={endpoint.name}
          {...attributes}
          {...listeners}
        >
          <GripVertical className="h-4 w-4" />
        </button>

        <button
          type="button"
          onClick={onToggleExpanded}
          className="min-w-0 flex-1 text-left"
        >
          <div className="flex items-center gap-2">
            <span className="truncate font-semibold">{endpoint.name}</span>
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
              {t("apiAccess.priority", { value: endpoint.priority })}
            </span>
          </div>
          <p className="mt-1 truncate font-mono text-xs text-muted-foreground">
            {endpoint.baseUrl}
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            {endpoint.models.length > 0
              ? t("apiAccess.modelCount", { count: endpoint.models.length })
              : t("apiAccess.modelAny")}
            {" · "}
            {t("apiAccess.enabledKeyCount", {
              enabled: endpoint.enabledKeyCount,
              total: endpoint.keyCount,
            })}
          </p>
          {endpoint.enabledKeyCount === 0 && (
            <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">
              {t("apiAccess.incomplete")}
            </p>
          )}
        </button>

        <div className="flex shrink-0 items-center gap-2">
          <span className="text-xs text-muted-foreground">
            {endpoint.enabled
              ? t("apiAccess.enabled")
              : t("apiAccess.disabled")}
          </span>
          <Switch
            checked={endpoint.enabled}
            onCheckedChange={onToggleEnabled}
            aria-label={endpoint.name}
          />
          <Button variant="ghost" size="icon" onClick={onEdit}>
            <Pencil className="h-4 w-4" />
            <span className="sr-only">{t("apiAccess.edit")}</span>
          </Button>
          <Button
            variant="ghost"
            size="icon"
            onClick={onDelete}
            className="text-destructive hover:text-destructive"
          >
            <Trash2 className="h-4 w-4" />
            <span className="sr-only">{t("apiAccess.delete")}</span>
          </Button>
        </div>
      </div>

      {expanded && (
        <div className="border-t border-border/40 bg-muted/30 px-4 py-3 pl-11">
          <div className="mb-2 flex items-center justify-between">
            <span className="text-xs font-medium text-muted-foreground">
              {t("apiAccess.keys.title")}
            </span>
            <Button variant="outline" size="sm" onClick={onAddKey}>
              <KeyRound className="mr-1.5 h-3.5 w-3.5" />
              {t("apiAccess.keys.add")}
            </Button>
          </div>

          {!keys || keys.length === 0 ? (
            <p className="py-2 text-xs text-muted-foreground">
              {t("apiAccess.keys.empty")}
            </p>
          ) : (
            <ul className="flex flex-col divide-y divide-border/40">
              {keys.map((key) => {
                const penalty = keyPenalty(key, nowSecs);
                return (
                  <li
                    key={key.id}
                    className="flex items-center gap-3 py-2 text-xs"
                  >
                    <span className="font-mono">••••{key.keyLast4}</span>
                    {key.name && (
                      <span className="truncate text-muted-foreground">
                        {key.name}
                      </span>
                    )}
                    <span className="text-muted-foreground/70">
                      {t("apiAccess.priority", { value: key.internalPriority })}
                    </span>
                    <span className="text-muted-foreground/70">
                      {t("apiAccess.keys.stats", {
                        success: key.successCount,
                        total: key.requestCount,
                      })}
                    </span>
                    {penalty && (
                      <span className="rounded bg-destructive/10 px-1.5 py-0.5 text-destructive">
                        {t(
                          `apiAccess.${penalty === "cooldown" ? "cooldown" : penalty === "quota_exhausted" ? "quotaExhausted" : penalty === "auth_invalid" ? "authInvalid" : "banned"}`,
                        )}
                      </span>
                    )}
                    <div className="ml-auto flex items-center gap-1.5">
                      {penalty && (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => onClearKeyPenalty(key)}
                        >
                          <RotateCcw className="h-3.5 w-3.5" />
                          <span className="sr-only">
                            {t("apiAccess.clearPenalty")}
                          </span>
                        </Button>
                      )}
                      <Switch
                        checked={key.enabled}
                        onCheckedChange={(next) =>
                          onToggleKeyEnabled(key, next)
                        }
                        aria-label={key.keyLast4}
                      />
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-destructive hover:text-destructive"
                        onClick={() => onDeleteKey(key)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                        <span className="sr-only">{t("apiAccess.delete")}</span>
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
