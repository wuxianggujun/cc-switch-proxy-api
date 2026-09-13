import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiGatewayApi } from "@/lib/api/apiGateway";
import type {
  NewApiEndpointWithKey,
  NewApiKey,
  UpdateApiEndpointInput,
  UpstreamType,
} from "@/types/apiGateway";

export const apiGatewayKeys = {
  all: ["apiGateway"] as const,
  endpoints: ["apiGateway", "endpoints"] as const,
  keys: (endpointId: string) => ["apiGateway", "keys", endpointId] as const,
  candidates: (upstreamType: UpstreamType, model?: string) =>
    ["apiGateway", "candidates", upstreamType, model ?? null] as const,
};

export function useApiEndpoints() {
  return useQuery({
    queryKey: apiGatewayKeys.endpoints,
    queryFn: () => apiGatewayApi.listEndpoints(),
  });
}

export function useApiKeys(endpointId: string | undefined) {
  return useQuery({
    queryKey: apiGatewayKeys.keys(endpointId ?? ""),
    queryFn: () => apiGatewayApi.listKeys(endpointId as string),
    enabled: Boolean(endpointId),
  });
}

export function useRouteCandidates(
  upstreamType: UpstreamType | undefined,
  model?: string,
) {
  return useQuery({
    queryKey: apiGatewayKeys.candidates(upstreamType ?? "openai", model),
    queryFn: () =>
      apiGatewayApi.previewRouteCandidates(upstreamType as UpstreamType, model),
    enabled: Boolean(upstreamType),
  });
}

/** 所有写操作共用：整棵子树失效。接入点与密钥的计数相互影响，分开失效容易漏。 */
function useGatewayMutation<TResult, TArgs>(
  fn: (args: TArgs) => Promise<TResult>,
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: apiGatewayKeys.all });
    },
  });
}

export function useCreateApiEndpoint() {
  return useGatewayMutation((input: NewApiEndpointWithKey) =>
    apiGatewayApi.createEndpoint(input),
  );
}

export function useUpdateApiEndpoint() {
  return useGatewayMutation((input: UpdateApiEndpointInput) =>
    apiGatewayApi.updateEndpoint(input),
  );
}

export function useDeleteApiEndpoint() {
  return useGatewayMutation((endpointId: string) =>
    apiGatewayApi.deleteEndpoint(endpointId),
  );
}

export function useReorderApiEndpoints() {
  return useGatewayMutation((endpointIds: string[]) =>
    apiGatewayApi.reorderEndpoints(endpointIds),
  );
}

export function useSetApiEndpointEnabled() {
  return useGatewayMutation(
    ({ endpointId, enabled }: { endpointId: string; enabled: boolean }) =>
      apiGatewayApi.setEndpointEnabled(endpointId, enabled),
  );
}

export function useCreateApiKey() {
  return useGatewayMutation((key: NewApiKey) => apiGatewayApi.createKey(key));
}

export function useDeleteApiKey() {
  return useGatewayMutation((keyId: string) => apiGatewayApi.deleteKey(keyId));
}

export function useSetApiKeyEnabled() {
  return useGatewayMutation(
    ({ keyId, enabled }: { keyId: string; enabled: boolean }) =>
      apiGatewayApi.setKeyEnabled(keyId, enabled),
  );
}

export function useClearApiKeyPenalty() {
  return useGatewayMutation((keyId: string) =>
    apiGatewayApi.clearKeyPenalty(keyId),
  );
}
