import { useMemo, useState } from "react";
import { Check, Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import type { FetchedModel } from "@/lib/api/model-fetch";

interface EndpointModelPickerProps {
  models: FetchedModel[];
  selectedIds: Set<string>;
  onToggle: (id: string) => void;
  onSelectAll: () => void;
  onClear: () => void;
}

export function EndpointModelPicker({
  models,
  selectedIds,
  onToggle,
  onSelectAll,
  onClear,
}: EndpointModelPickerProps) {
  const { t } = useTranslation();
  const [search, setSearch] = useState("");
  const visible = useMemo(() => {
    const needle = search.trim().toLowerCase();
    if (!needle) return models;
    return models.filter(
      (model) =>
        model.id.toLowerCase().includes(needle) ||
        (model.ownedBy ?? "").toLowerCase().includes(needle),
    );
  }, [models, search]);
  const selectedCount = models.filter((model) =>
    selectedIds.has(model.id),
  ).length;

  return (
    <div className="overflow-hidden rounded-md border border-border/70">
      <div className="flex items-center justify-between gap-2 border-b border-border/60 bg-muted/30 px-2 py-1.5">
        <span className="text-xs text-muted-foreground">
          {t("apiAccess.form.modelsSelected", {
            selected: selectedCount,
            total: models.length,
          })}
        </span>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs"
            onClick={onSelectAll}
          >
            {t("apiAccess.form.modelsSelectAll")}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 px-2 text-xs"
            onClick={onClear}
          >
            {t("apiAccess.form.modelsClearFetched")}
          </Button>
        </div>
      </div>
      <div className="relative border-b border-border/60">
        <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder={t("apiAccess.form.modelsSearch")}
          className="h-8 rounded-none border-0 pl-8 shadow-none focus-visible:ring-0"
        />
      </div>
      <div className="max-h-44 overflow-y-auto p-1">
        {visible.length === 0 ? (
          <p className="px-2 py-4 text-center text-xs text-muted-foreground">
            {t("apiAccess.form.modelsNoResults")}
          </p>
        ) : (
          visible.map((model) => {
            const selected = selectedIds.has(model.id);
            return (
              <button
                key={model.id}
                type="button"
                onClick={() => onToggle(model.id)}
                className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm hover:bg-accent"
              >
                <Check
                  className={cn(
                    "h-4 w-4 shrink-0",
                    selected ? "opacity-100" : "opacity-0",
                  )}
                />
                <span className="min-w-0 flex-1 truncate font-mono text-xs">
                  {model.id}
                </span>
                {model.ownedBy && (
                  <span className="max-w-32 truncate text-[11px] text-muted-foreground">
                    {model.ownedBy}
                  </span>
                )}
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}
