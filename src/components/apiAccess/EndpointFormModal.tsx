import { useEffect, useMemo, useRef, useState } from "react";
import { Download, Eye, EyeOff, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
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
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { apiGatewayApi } from "@/lib/api/apiGateway";
import {
  normalizeApiEndpointUrl,
  validateApiEndpointUrl,
} from "@/lib/api/apiGatewayValidation";
import { showFetchModelsError, type FetchedModel } from "@/lib/api/model-fetch";
import {
  UPSTREAM_LABELS,
  UPSTREAM_ORDER,
  type ApiEndpoint,
  type UpstreamType,
} from "@/types/apiGateway";
import { EndpointModelPicker } from "./EndpointModelPicker";

export interface EndpointFormValues {
  name: string;
  upstreamType: UpstreamType;
  baseUrl: string;
  apiKey?: string;
  models: string[];
  priority: number;
  notes?: string;
}

interface EndpointFormModalProps {
  open: boolean;
  endpoint?: ApiEndpoint;
  defaultUpstreamType: UpstreamType;
  pending?: boolean;
  onSubmit: (values: EndpointFormValues) => Promise<void> | void;
  onCancel: () => void;
}

function parseModels(value: string): string[] {
  const seen = new Set<string>();
  const models: string[] = [];
  for (const line of value.split("\n")) {
    const model = line.trim();
    if (!model || seen.has(model)) continue;
    seen.add(model);
    models.push(model);
  }
  return models;
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
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [modelsText, setModelsText] = useState("");
  const [priority, setPriority] = useState("100");
  const [notes, setNotes] = useState("");
  const [fetchedModels, setFetchedModels] = useState<FetchedModel[]>([]);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const requestSequence = useRef(0);
  const submitLock = useRef(false);

  useEffect(() => {
    if (!open) {
      requestSequence.current += 1;
      setApiKey("");
      setShowKey(false);
      return;
    }
    setName(endpoint?.name ?? "");
    setUpstreamType(endpoint?.upstreamType ?? defaultUpstreamType);
    setBaseUrl(endpoint?.baseUrl ?? "");
    setApiKey("");
    setShowKey(false);
    setModelsText((endpoint?.models ?? []).join("\n"));
    setPriority(String(endpoint?.priority ?? 100));
    setNotes(endpoint?.notes ?? "");
    setFetchedModels([]);
    setFetchingModels(false);
    setSubmitted(false);
    submitLock.current = false;
    requestSequence.current += 1;
  }, [open, endpoint, defaultUpstreamType]);

  const trimmedName = name.trim();
  const trimmedUrl = baseUrl.trim();
  const normalizedUrl = normalizeApiEndpointUrl(baseUrl);
  const trimmedKey = apiKey.trim();
  const parsedPriority = Number(priority);
  const urlError = validateApiEndpointUrl(baseUrl);
  const targetChanged = Boolean(
    endpoint &&
      (normalizedUrl !== endpoint.baseUrl ||
        upstreamType !== endpoint.upstreamType),
  );
  const keyRequired = !endpoint || targetChanged;
  const priorityValid =
    priority.trim().length > 0 && Number.isSafeInteger(parsedPriority);
  const canSubmit =
    trimmedName.length > 0 &&
    !urlError &&
    priorityValid &&
    (!keyRequired || trimmedKey.length > 0) &&
    !pending;

  const selectedIds = useMemo(
    () => new Set(parseModels(modelsText)),
    [modelsText],
  );

  const invalidateFetchedModels = () => {
    requestSequence.current += 1;
    setFetchedModels([]);
    setFetchingModels(false);
  };

  const updateModels = (models: string[]) => setModelsText(models.join("\n"));

  const toggleFetchedModel = (id: string) => {
    const models = parseModels(modelsText);
    updateModels(
      models.includes(id)
        ? models.filter((model) => model !== id)
        : [...models, id],
    );
  };

  const selectAllFetched = () => {
    const models = parseModels(modelsText);
    const seen = new Set(models);
    updateModels([
      ...models,
      ...fetchedModels.map((model) => model.id).filter((id) => !seen.has(id)),
    ]);
  };

  const clearFetched = () => {
    const fetchedIds = new Set(fetchedModels.map((model) => model.id));
    updateModels(
      parseModels(modelsText).filter((model) => !fetchedIds.has(model)),
    );
  };

  const fetchModels = async () => {
    if (urlError) {
      setSubmitted(true);
      toast.error(t(`apiAccess.form.urlErrors.${urlError}`));
      return;
    }
    if (keyRequired && !trimmedKey) {
      setSubmitted(true);
      toast.error(t("apiAccess.form.apiKeyRequired"));
      return;
    }

    const sequence = ++requestSequence.current;
    setFetchingModels(true);
    try {
      const models = trimmedKey
        ? await apiGatewayApi.fetchDraftModels({
            baseUrl: trimmedUrl,
            apiKey: trimmedKey,
            upstreamType,
          })
        : endpoint
          ? await apiGatewayApi.fetchEndpointModels(endpoint.id)
          : [];
      if (sequence !== requestSequence.current) return;
      setFetchedModels(models);
      if (models.length === 0) {
        toast.info(t("apiAccess.form.modelsEmptyResult"));
      } else {
        toast.success(
          t("apiAccess.form.modelsFetched", { count: models.length }),
        );
      }
    } catch (error) {
      if (sequence !== requestSequence.current) return;
      showFetchModelsError(error, t, {
        hasApiKey: Boolean(trimmedKey || endpoint),
        hasBaseUrl: Boolean(trimmedUrl),
      });
    } finally {
      if (sequence === requestSequence.current) setFetchingModels(false);
    }
  };

  const submit = async () => {
    setSubmitted(true);
    if (!canSubmit || submitLock.current) return;
    submitLock.current = true;
    try {
      await onSubmit({
        name: trimmedName,
        upstreamType,
        baseUrl: trimmedUrl,
        apiKey: trimmedKey || undefined,
        models: parseModels(modelsText),
        priority: parsedPriority,
        notes: notes.trim() || undefined,
      });
    } finally {
      submitLock.current = false;
    }
  };

  const errorText = (key: string) => (
    <p className="text-xs text-destructive">{t(key)}</p>
  );

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>
            {endpoint
              ? t("apiAccess.form.editTitle")
              : t("apiAccess.form.addTitle")}
          </DialogTitle>
        </DialogHeader>

        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-6 py-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-name">{t("apiAccess.form.name")}</Label>
            <Input
              id="ep-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder={t("apiAccess.form.namePlaceholder")}
            />
            {submitted &&
              !trimmedName &&
              errorText("apiAccess.form.nameRequired")}
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-upstream">
              {t("apiAccess.form.upstreamType")}
            </Label>
            <Select
              value={upstreamType}
              onValueChange={(next) => {
                setUpstreamType(next as UpstreamType);
                invalidateFetchedModels();
              }}
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
              onChange={(event) => {
                setBaseUrl(event.target.value);
                invalidateFetchedModels();
              }}
              placeholder="https://api.example.com/v1"
              className="font-mono text-sm"
              aria-invalid={submitted && Boolean(urlError)}
            />
            {submitted &&
              urlError &&
              errorText(`apiAccess.form.urlErrors.${urlError}`)}
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-key">{t("apiAccess.form.apiKey")}</Label>
            <div className="relative">
              <Input
                id="ep-key"
                type={showKey ? "text" : "password"}
                value={apiKey}
                onChange={(event) => {
                  setApiKey(event.target.value);
                  invalidateFetchedModels();
                }}
                placeholder={
                  endpoint && !targetChanged
                    ? t("apiAccess.form.apiKeyKeepPlaceholder")
                    : t("apiAccess.form.apiKeyPlaceholder")
                }
                className="pr-10 font-mono text-sm"
                autoComplete="new-password"
                spellCheck={false}
                aria-invalid={submitted && keyRequired && !trimmedKey}
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
            {submitted &&
              keyRequired &&
              !trimmedKey &&
              errorText("apiAccess.form.apiKeyRequired")}
            {targetChanged ? (
              <p className="text-xs text-amber-600 dark:text-amber-400">
                {t("apiAccess.form.rebindKeyHint")}
              </p>
            ) : endpoint ? (
              <p className="text-xs text-muted-foreground">
                {t("apiAccess.form.apiKeyEditHint")}
              </p>
            ) : (
              <p className="text-xs text-muted-foreground">
                {t("apiAccess.form.apiKeyCreateHint")}
              </p>
            )}
          </div>

          <div className="flex flex-col gap-1.5">
            <div className="flex items-center justify-between gap-3">
              <Label htmlFor="ep-models">{t("apiAccess.form.models")}</Label>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="h-8"
                onClick={fetchModels}
                disabled={fetchingModels}
              >
                {fetchingModels ? (
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Download className="mr-1.5 h-3.5 w-3.5" />
                )}
                {fetchingModels
                  ? t("apiAccess.form.modelsFetching")
                  : t("apiAccess.form.modelsFetch")}
              </Button>
            </div>
            <Textarea
              id="ep-models"
              value={modelsText}
              onChange={(event) => setModelsText(event.target.value)}
              rows={4}
              placeholder={t("apiAccess.form.modelsPlaceholder")}
              className="font-mono text-xs"
            />
            <p className="text-xs text-muted-foreground">
              {t("apiAccess.form.modelsHint")}
            </p>
            {fetchedModels.length > 0 && (
              <EndpointModelPicker
                models={fetchedModels}
                selectedIds={selectedIds}
                onToggle={toggleFetchedModel}
                onSelectAll={selectAllFetched}
                onClear={clearFetched}
              />
            )}
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-priority">{t("apiAccess.form.priority")}</Label>
            <Input
              id="ep-priority"
              type="number"
              step="1"
              value={priority}
              onChange={(event) => setPriority(event.target.value)}
              aria-invalid={submitted && !priorityValid}
            />
            {submitted &&
              !priorityValid &&
              errorText("apiAccess.form.priorityInteger")}
            <p className="text-xs text-muted-foreground">
              {t("apiAccess.form.priorityHint")}
            </p>
          </div>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="ep-notes">{t("apiAccess.form.notes")}</Label>
            <Textarea
              id="ep-notes"
              value={notes}
              onChange={(event) => setNotes(event.target.value)}
              rows={2}
            />
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
