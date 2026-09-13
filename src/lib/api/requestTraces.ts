import { invoke } from "@tauri-apps/api/core";
import type {
  RequestTraceConfig,
  RequestTraceDetail,
  RequestTraceFilters,
  RequestTracePage,
} from "@/types/requestTrace";

export const requestTracesApi = {
  list: (filters: RequestTraceFilters, page: number, pageSize: number) =>
    invoke<RequestTracePage>("list_request_traces", {
      filters,
      page,
      pageSize,
    }),
  detail: (requestId: string) =>
    invoke<RequestTraceDetail | null>("get_request_trace", { requestId }),
  config: () => invoke<RequestTraceConfig>("get_request_trace_config"),
  saveConfig: (config: RequestTraceConfig) =>
    invoke<void>("set_request_trace_config", { config }),
  clear: () => invoke<number>("clear_request_traces"),
};
