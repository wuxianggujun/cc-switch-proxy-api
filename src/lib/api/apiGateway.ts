import { invoke } from "@tauri-apps/api/core";
import type {
  ApiEndpoint,
  ApiKey,
  CreatedApiEndpoint,
  GatewayModelFetchDraft,
  KeyMutationOutcome,
  NewApiEndpointWithKey,
  NewApiKey,
  RouteCandidate,
  UpdatedApiEndpoint,
  UpdateApiEndpointInput,
  UpstreamType,
} from "@/types/apiGateway";
import type { FetchedModel } from "@/lib/api/model-fetch";

export const apiGatewayApi = {
  // ── 接入点 ──────────────────────────────────────────

  listEndpoints(): Promise<ApiEndpoint[]> {
    return invoke("list_api_endpoints");
  },

  createEndpoint(input: NewApiEndpointWithKey): Promise<CreatedApiEndpoint> {
    return invoke("create_api_endpoint_with_key", { input });
  },

  updateEndpoint(input: UpdateApiEndpointInput): Promise<UpdatedApiEndpoint> {
    return invoke("update_api_endpoint", { input });
  },

  fetchDraftModels(input: GatewayModelFetchDraft): Promise<FetchedModel[]> {
    return invoke("fetch_api_gateway_draft_models", { input });
  },

  fetchEndpointModels(endpointId: string): Promise<FetchedModel[]> {
    return invoke("fetch_api_endpoint_models", { endpointId });
  },

  deleteEndpoint(endpointId: string): Promise<void> {
    return invoke("delete_api_endpoint", { endpointId });
  },

  /** 拖拽排序。仅影响展示序，不影响路由优先级。 */
  reorderEndpoints(endpointIds: string[]): Promise<void> {
    return invoke("reorder_api_endpoints", { endpointIds });
  },

  setEndpointEnabled(endpointId: string, enabled: boolean): Promise<void> {
    return invoke("set_api_endpoint_enabled", { endpointId, enabled });
  },

  // ── 密钥 ────────────────────────────────────────────

  listKeys(endpointId: string): Promise<ApiKey[]> {
    return invoke("list_api_keys", { endpointId });
  },

  createKey(key: NewApiKey): Promise<string> {
    return invoke("create_api_key", { key });
  },

  deleteKey(keyId: string): Promise<KeyMutationOutcome> {
    return invoke("delete_api_key", { keyId });
  },

  setKeyEnabled(keyId: string, enabled: boolean): Promise<KeyMutationOutcome> {
    return invoke("set_api_key_enabled", { keyId, enabled });
  },

  /** 清除冷却与硬状态，让密钥立刻重新参与选线。 */
  clearKeyPenalty(keyId: string): Promise<void> {
    return invoke("clear_api_key_penalty", { keyId });
  },

  /** 预览当前生效的选线顺序。 */
  previewRouteCandidates(
    upstreamType: UpstreamType,
    model?: string,
  ): Promise<RouteCandidate[]> {
    return invoke("preview_route_candidates", { upstreamType, model });
  },
};
