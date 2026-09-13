import { beforeEach, describe, expect, it, vi } from "vitest";
import { requestTracesApi } from "@/lib/api/requestTraces";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

describe("request trace command boundary", () => {
  beforeEach(() => {
    invoke.mockReset();
  });
  it("passes filters and zero-based pagination without interpolating a query", async () => {
    invoke.mockResolvedValue({ data: [], total: 0, storageBytes: 0 });
    const filters = {
      query: "中文 100% ' OR 1=1",
      clientIp: "::1",
      errorsOnly: true,
    };
    await requestTracesApi.list(filters, 2, 25);
    expect(invoke).toHaveBeenCalledWith("list_request_traces", {
      filters,
      page: 2,
      pageSize: 25,
    });
  });
  it("gets a trace by ID and propagates failures", async () => {
    invoke.mockRejectedValue(new Error("database unavailable"));
    await expect(requestTracesApi.detail("trace-1")).rejects.toThrow(
      "database unavailable",
    );
    expect(invoke).toHaveBeenCalledWith("get_request_trace", {
      requestId: "trace-1",
    });
  });
  it("uses separate diagnostic settings and clear commands", async () => {
    invoke.mockResolvedValue(undefined);
    const config = {
      enabled: true,
      captureBodies: false,
      maxBodyBytes: 4096,
      retentionDays: 3,
      maxEntries: 100,
      maxStorageMb: 32,
    };
    await requestTracesApi.saveConfig(config);
    expect(invoke).toHaveBeenCalledWith("set_request_trace_config", { config });
    await requestTracesApi.clear();
    expect(invoke).toHaveBeenLastCalledWith("clear_request_traces");
  });
});
