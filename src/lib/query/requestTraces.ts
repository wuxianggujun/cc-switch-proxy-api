import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { requestTracesApi } from "@/lib/api/requestTraces";
import type {
  RequestTraceConfig,
  RequestTraceFilters,
} from "@/types/requestTrace";

const rootKey = ["request-traces"] as const;
export const requestTraceKeys = {
  all: rootKey,
  config: [...rootKey, "config"] as const,
};

export function useRequestTraces(
  filters: RequestTraceFilters,
  page: number,
  live: boolean,
) {
  return useQuery({
    queryKey: [...rootKey, "list", filters, page],
    queryFn: () => requestTracesApi.list(filters, page, 25),
    refetchInterval: live ? 2000 : false,
  });
}

export function useRequestTrace(requestId: string | null) {
  return useQuery({
    queryKey: [...rootKey, "detail", requestId],
    queryFn: () => requestTracesApi.detail(requestId!),
    enabled: !!requestId,
    refetchInterval: (query) =>
      query.state.data?.state === "in_progress" ? 1500 : false,
    gcTime: 0,
    staleTime: 0,
  });
}

export function useRequestTraceConfig() {
  return useQuery({
    queryKey: requestTraceKeys.config,
    queryFn: requestTracesApi.config,
  });
}

export function useSaveRequestTraceConfig() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (config: RequestTraceConfig) =>
      requestTracesApi.saveConfig(config),
    onSuccess: () => client.invalidateQueries({ queryKey: rootKey }),
  });
}

export function useClearRequestTraces() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: requestTracesApi.clear,
    onSuccess: async () => {
      client.removeQueries({ queryKey: [...rootKey, "detail"] });
      await client.invalidateQueries({ queryKey: [...rootKey, "list"] });
    },
  });
}
