import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { Plus, RefreshCw, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  useApiEndpoints,
  useApiKeys,
  useCreateApiEndpoint,
  useCreateApiKey,
  useDeleteApiEndpoint,
  useDeleteApiKey,
  useReorderApiEndpoints,
  useSetApiEndpointEnabled,
  useSetApiKeyEnabled,
  useClearApiKeyPenalty,
  useUpdateApiEndpoint,
} from "@/lib/query/apiGateway";
import {
  UPSTREAM_LABELS,
  type ApiEndpoint,
  type ApiKey,
  type UpstreamType,
} from "@/types/apiGateway";
import { UpstreamColumn } from "./UpstreamColumn";
import { GatewayStatusBar } from "./GatewayStatusBar";
import { EndpointRow } from "./EndpointRow";
import {
  EndpointFormModal,
  type EndpointFormValues,
} from "./EndpointFormModal";
import { KeyFormModal, type KeyFormValues } from "./KeyFormModal";

export function ApiAccessPage() {
  const { t } = useTranslation();
  const [selectedUpstream, setSelectedUpstream] =
    useState<UpstreamType>("claude");
  const [search, setSearch] = useState("");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<ApiEndpoint | undefined>();
  const [keyFormEndpointId, setKeyFormEndpointId] = useState<string | null>(
    null,
  );
  const [deleteTarget, setDeleteTarget] = useState<ApiEndpoint | null>(null);
  const [deleteKeyTarget, setDeleteKeyTarget] = useState<ApiKey | null>(null);

  const {
    data: endpoints = [],
    isLoading,
    refetch,
    isFetching,
  } = useApiEndpoints();
  const { data: expandedKeys } = useApiKeys(expandedId ?? undefined);

  const createEndpoint = useCreateApiEndpoint();
  const updateEndpoint = useUpdateApiEndpoint();
  const deleteEndpoint = useDeleteApiEndpoint();
  const reorderEndpoints = useReorderApiEndpoints();
  const setEndpointEnabled = useSetApiEndpointEnabled();
  const createKey = useCreateApiKey();
  const deleteKey = useDeleteApiKey();
  const setKeyEnabled = useSetApiKeyEnabled();
  const clearPenalty = useClearApiKeyPenalty();

  // 8px 激活距离：避免点击展开/开关时误触发拖拽
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    }),
  );

  // 当前上游下的接入，再按搜索词过滤。搜索覆盖备注名、地址和末四位。
  const visible = useMemo(() => {
    const scoped = endpoints.filter(
      (endpoint) => endpoint.upstreamType === selectedUpstream,
    );
    const needle = search.trim().toLowerCase();
    if (!needle) return scoped;
    return scoped.filter(
      (endpoint) =>
        endpoint.name.toLowerCase().includes(needle) ||
        endpoint.baseUrl.toLowerCase().includes(needle) ||
        (endpoint.notes ?? "").toLowerCase().includes(needle),
    );
  }, [endpoints, selectedUpstream, search]);

  const reportFailure = (error: unknown) => {
    toast.error(t("apiAccess.toast.failed", { error: String(error) }));
  };

  const reportAutoDisabled = (endpointAutoDisabled: boolean) => {
    if (endpointAutoDisabled) {
      toast.warning(t("apiAccess.toast.endpointAutoDisabled"));
    }
  };

  const handleDragEnd = async (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;

    const oldIndex = visible.findIndex((item) => item.id === active.id);
    const newIndex = visible.findIndex((item) => item.id === over.id);
    if (oldIndex < 0 || newIndex < 0) return;

    // 只重排当前可见子集，再把它拼回全量顺序：跨上游拖拽不存在，
    // 但被搜索过滤掉的项必须保持原有相对位置。
    const reordered = [...visible];
    const [moved] = reordered.splice(oldIndex, 1);
    reordered.splice(newIndex, 0, moved);

    const movedIds = new Set(reordered.map((item) => item.id));
    let cursor = 0;
    const fullOrder = endpoints.map((item) =>
      movedIds.has(item.id) ? reordered[cursor++].id : item.id,
    );

    try {
      await reorderEndpoints.mutateAsync(fullOrder);
    } catch (error) {
      reportFailure(error);
    }
  };

  const submitEndpoint = async (values: EndpointFormValues) => {
    try {
      const { apiKey, ...endpointValues } = values;
      let endpointId: string;

      if (editing) {
        const result = await updateEndpoint.mutateAsync({
          endpointId: editing.id,
          ...endpointValues,
          ...(apiKey ? { newKey: { apiKey } } : {}),
        });
        endpointId = result.endpointId;
        toast.success(t("apiAccess.toast.updated"));
      } else {
        if (!apiKey) return;
        const result = await createEndpoint.mutateAsync({
          endpoint: endpointValues,
          firstKey: { apiKey },
        });
        endpointId = result.endpointId;
        toast.success(t("apiAccess.toast.created"));
      }

      setSelectedUpstream(values.upstreamType);
      setExpandedId(endpointId);
      setFormOpen(false);
      setEditing(undefined);
    } catch (error) {
      reportFailure(error);
    }
  };

  const submitKey = async (values: KeyFormValues) => {
    if (!keyFormEndpointId) return;
    try {
      await createKey.mutateAsync({
        endpointId: keyFormEndpointId,
        ...values,
      });
      toast.success(t("apiAccess.toast.keyCreated"));
      setKeyFormEndpointId(null);
    } catch (error) {
      reportFailure(error);
    }
  };

  const confirmDeleteEndpoint = async () => {
    if (!deleteTarget) return;
    try {
      await deleteEndpoint.mutateAsync(deleteTarget.id);
      toast.success(t("apiAccess.toast.deleted"));
      if (expandedId === deleteTarget.id) setExpandedId(null);
    } catch (error) {
      reportFailure(error);
    } finally {
      setDeleteTarget(null);
    }
  };

  const confirmDeleteKey = async () => {
    if (!deleteKeyTarget) return;
    try {
      const outcome = await deleteKey.mutateAsync(deleteKeyTarget.id);
      toast.success(t("apiAccess.toast.keyDeleted"));
      reportAutoDisabled(outcome.endpointAutoDisabled);
    } catch (error) {
      reportFailure(error);
    } finally {
      setDeleteKeyTarget(null);
    }
  };

  return (
    <div className="mx-auto flex w-full max-w-[1500px] flex-col gap-4 px-6 py-6">
      <GatewayStatusBar
        upstream={selectedUpstream}
        actions={
          <>
            <span className="text-sm text-muted-foreground">
              {t("apiAccess.count", { count: endpoints.length })}
            </span>
            <Button
              variant="outline"
              onClick={() => refetch()}
              disabled={isFetching}
            >
              <RefreshCw
                className={
                  isFetching ? "mr-2 h-4 w-4 animate-spin" : "mr-2 h-4 w-4"
                }
              />
              {t("apiAccess.refresh")}
            </Button>
            <Button
              onClick={() => {
                setEditing(undefined);
                setFormOpen(true);
              }}
            >
              <Plus className="mr-2 h-4 w-4" />
              {t("apiAccess.add")}
            </Button>
          </>
        }
      />

      <div className="flex gap-4">
        <UpstreamColumn
          endpoints={endpoints}
          selected={selectedUpstream}
          onSelect={(type) => {
            setSelectedUpstream(type);
            setExpandedId(null);
          }}
        />

        <section className="min-w-0 flex-1 rounded-xl border border-border/60 bg-card">
          <div className="flex items-center justify-between gap-4 border-b border-border/50 px-4 py-3">
            <div>
              <h2 className="font-semibold">
                {UPSTREAM_LABELS[selectedUpstream]}
              </h2>
              <p className="text-xs text-muted-foreground">
                {t("apiAccess.matched", { count: visible.length })}
              </p>
            </div>
            <div className="relative w-72">
              <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t("apiAccess.searchPlaceholder")}
                className="pl-9"
              />
            </div>
          </div>

          {isLoading ? (
            <p className="px-4 py-10 text-center text-sm text-muted-foreground">
              …
            </p>
          ) : visible.length === 0 ? (
            <p className="px-4 py-10 text-center text-sm text-muted-foreground">
              {search.trim()
                ? t("apiAccess.emptyFiltered")
                : t("apiAccess.empty")}
            </p>
          ) : (
            <DndContext
              sensors={sensors}
              collisionDetection={closestCenter}
              onDragEnd={handleDragEnd}
            >
              <SortableContext
                items={visible.map((item) => item.id)}
                strategy={verticalListSortingStrategy}
              >
                {visible.map((endpoint) => (
                  <EndpointRow
                    key={endpoint.id}
                    endpoint={endpoint}
                    keys={expandedId === endpoint.id ? expandedKeys : undefined}
                    expanded={expandedId === endpoint.id}
                    onToggleExpanded={() =>
                      setExpandedId(
                        expandedId === endpoint.id ? null : endpoint.id,
                      )
                    }
                    onToggleEnabled={(enabled) => {
                      if (enabled && endpoint.enabledKeyCount === 0) {
                        toast.error(t("apiAccess.toast.enableNeedsKey"));
                        return;
                      }
                      setEndpointEnabled
                        .mutateAsync({ endpointId: endpoint.id, enabled })
                        .catch(reportFailure);
                    }}
                    onEdit={() => {
                      setEditing(endpoint);
                      setFormOpen(true);
                    }}
                    onDelete={() => setDeleteTarget(endpoint)}
                    onAddKey={() => setKeyFormEndpointId(endpoint.id)}
                    onDeleteKey={(key) => setDeleteKeyTarget(key)}
                    onToggleKeyEnabled={(key, enabled) =>
                      setKeyEnabled
                        .mutateAsync({ keyId: key.id, enabled })
                        .then((outcome) =>
                          reportAutoDisabled(outcome.endpointAutoDisabled),
                        )
                        .catch(reportFailure)
                    }
                    onClearKeyPenalty={(key) =>
                      clearPenalty
                        .mutateAsync(key.id)
                        .then(() =>
                          toast.success(t("apiAccess.toast.penaltyCleared")),
                        )
                        .catch(reportFailure)
                    }
                  />
                ))}
              </SortableContext>
            </DndContext>
          )}
        </section>
      </div>

      <EndpointFormModal
        open={formOpen}
        endpoint={editing}
        defaultUpstreamType={selectedUpstream}
        pending={createEndpoint.isPending || updateEndpoint.isPending}
        onSubmit={submitEndpoint}
        onCancel={() => {
          setFormOpen(false);
          setEditing(undefined);
        }}
      />

      <KeyFormModal
        open={keyFormEndpointId !== null}
        pending={createKey.isPending}
        onSubmit={submitKey}
        onCancel={() => setKeyFormEndpointId(null)}
      />

      <ConfirmDialog
        isOpen={deleteTarget !== null}
        title={t("apiAccess.deleteConfirm.title")}
        message={t("apiAccess.deleteConfirm.message", {
          name: deleteTarget?.name ?? "",
        })}
        variant="destructive"
        pending={deleteEndpoint.isPending}
        onConfirm={confirmDeleteEndpoint}
        onCancel={() => setDeleteTarget(null)}
      />

      <ConfirmDialog
        isOpen={deleteKeyTarget !== null}
        title={t("apiAccess.deleteKeyConfirm.title")}
        message={t("apiAccess.deleteKeyConfirm.message", {
          last4: deleteKeyTarget?.keyLast4 ?? "",
        })}
        variant="destructive"
        pending={deleteKey.isPending}
        onConfirm={confirmDeleteKey}
        onCancel={() => setDeleteKeyTarget(null)}
      />
    </div>
  );
}
