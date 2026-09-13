import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RequestLogsPage } from "@/components/requestLogs/RequestLogsPage";
import type { RequestTraceDetail, TracePayload } from "@/types/requestTrace";

const hooks = vi.hoisted(() => ({
  list: vi.fn(),
  detail: vi.fn(),
  config: vi.fn(),
  save: vi.fn(),
  clear: vi.fn(),
}));
vi.mock("@/lib/query/requestTraces", () => ({
  useRequestTraces: hooks.list,
  useRequestTrace: hooks.detail,
  useRequestTraceConfig: hooks.config,
  useSaveRequestTraceConfig: () => ({
    mutateAsync: hooks.save,
    isPending: false,
  }),
  useClearRequestTraces: () => ({ mutateAsync: hooks.clear, isPending: false }),
}));

const payload = (body: string): TracePayload => ({
  headers: [{ name: "authorization", value: "[REDACTED]" }],
  body,
  bodyBytes: 200,
  capturedBytes: 200,
  truncated: false,
  redacted: false,
  bodyEncoding: "utf8",
  captureError: null,
});
const detail: RequestTraceDetail = {
  requestId: "trace-1",
  startedAt: 1788900000000,
  method: "POST",
  path: "/v1/chat/completions",
  clientIp: "127.0.0.1",
  clientPort: 12345,
  entryProtocol: "openai_chat",
  model: "test-model",
  statusCode: 400,
  state: "error",
  durationMs: 42,
  firstByteMs: 30,
  attemptCount: 1,
  providerName: "Test upstream",
  request: payload(
    '{"messages":[{"role":"system","content":"原始系统提示词"},{"role":"user","content":"原始用户提示词"}]}',
  ),
  response: payload('{"error":{"message":"blocked by upstream"}}'),
  error: "blocked by upstream",
  bodyCaptureEnabled: true,
  attempts: [
    {
      index: 1,
      providerId: "gw:test",
      providerName: "Test upstream",
      protocol: "openai_responses",
      method: "POST",
      url: "https://upstream.example/v1/responses",
      proxy: null,
      model: "test-model",
      startedAt: 1788900000000,
      durationMs: 40,
      statusCode: 400,
      error: "upstream policy error",
      request: payload(
        '{"instructions":"原始系统提示词","input":"原始用户提示词"}',
      ),
      response: payload('{"error":{"message":"upstream policy error"}}'),
    },
  ],
};
const config = {
  enabled: true,
  captureBodies: true,
  retentionDays: 3,
  maxBodyBytes: 1048576,
  maxEntries: 1000,
  maxStorageMb: 128,
};

describe("RequestLogsPage", () => {
  beforeEach(() => {
    hooks.list.mockReturnValue({
      data: { data: [detail], total: 30, storageBytes: 2048 },
      isLoading: false,
      isFetching: false,
      refetch: vi.fn(),
    });
    hooks.detail.mockReturnValue({
      data: detail,
      isLoading: false,
      refetch: vi.fn(),
    });
    hooks.config.mockReturnValue({ data: config });
    hooks.save.mockResolvedValue(undefined);
    hooks.clear.mockResolvedValue(30);
  });
  it("shows socket IP and provider and sends search/pagination filters", () => {
    render(<RequestLogsPage />);
    expect(screen.getByText("127.0.0.1")).toBeInTheDocument();
    expect(screen.getByText("Test upstream")).toBeInTheDocument();
    fireEvent.change(
      screen.getByPlaceholderText("requestLogs.searchPlaceholder"),
      { target: { value: "拦截提示词" } },
    );
    fireEvent.change(screen.getByPlaceholderText("requestLogs.ipPlaceholder"), {
      target: { value: "::1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "requestLogs.search" }));
    expect(hooks.list).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: "拦截提示词", clientIp: "::1" }),
      0,
      true,
    );
    fireEvent.click(screen.getByRole("button", { name: "requestLogs.next" }));
    expect(hooks.list).toHaveBeenLastCalledWith(expect.any(Object), 1, true);
    fireEvent.click(
      screen.getByRole("checkbox", { name: "requestLogs.errorsOnly" }),
    );
    expect(hooks.list).toHaveBeenLastCalledWith(
      expect.objectContaining({ errorsOnly: true }),
      0,
      true,
    );
  });
  it("opens actual upstream payloads and compares prompts without HTML rendering", async () => {
    const user = userEvent.setup();
    render(<RequestLogsPage />);
    await user.click(
      screen.getByRole("button", { name: /POST \/v1\/chat\/completions/ }),
    );
    expect(hooks.detail).toHaveBeenLastCalledWith("trace-1");
    await user.click(
      screen.getByRole("tab", { name: /requestLogs.upstreamAttempts/ }),
    );
    expect(
      await screen.findByText("POST https://upstream.example/v1/responses"),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "requestLogs.prompts" }));
    expect(screen.getAllByText("原始用户提示词")).toHaveLength(2);
    expect(screen.getAllByText("原始系统提示词")).toHaveLength(2);
  });
  it("pauses refresh independently of persistent capture", async () => {
    render(<RequestLogsPage />);
    fireEvent.click(screen.getByRole("button", { name: "requestLogs.live" }));
    expect(hooks.list).toHaveBeenLastCalledWith(expect.any(Object), 0, false);
    expect(hooks.save).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("switch", { name: "requestLogs.enabled" }),
    );
    await waitFor(() =>
      expect(hooks.save).toHaveBeenCalledWith({ ...config, enabled: false }),
    );
  });
  it("does not report backend failures as an empty successful result", () => {
    hooks.list.mockReturnValue({
      error: new Error("database failed"),
      isLoading: false,
    });
    render(<RequestLogsPage />);
    expect(screen.getByRole("alert")).toHaveTextContent("database failed");
    expect(screen.queryByText("requestLogs.empty")).not.toBeInTheDocument();
  });
});
