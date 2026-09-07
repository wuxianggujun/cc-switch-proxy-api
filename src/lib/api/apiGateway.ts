import { invoke } from "@tauri-apps/api/core";
import type {
  ApiEndpoint,
  ApiKey,
  NewApiEndpoint,
  NewApiKey,
  RouteCandidate,
  UpstreamType,
} from "@/types/apiGateway";

export const apiGatewayApi = {
  // ── 接入点 ──────────────────────────────────────────

  listEndpoints(): Promise<ApiEndpoint[]> {
    return invoke("list_api_endpoints");
  },

  createEndpoint(endpoint: NewApiEndpoint): Promise<string> {
    return invoke("create_api_endpoint", { endpoint });
  },

  updateEndpoint(endpoint: ApiEndpoint): Promise<void> {
    return invoke("update_api_endpoint", { endpoint });
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

  deleteKey(keyId: string): Promise<void> {
    return invoke("delete_api_key", { keyId });
  },

  setKeyEnabled(keyId: string, enabled: boolean): Promise<void> {
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
