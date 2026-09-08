import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GatewayStatusBar } from "@/components/apiAccess/GatewayStatusBar";

const useProxyStatusMock = vi.hoisted(() => vi.fn());
vi.mock("@/hooks/useProxyStatus", () => ({
  useProxyStatus: useProxyStatusMock,
}));

describe("GatewayStatusBar", () => {
  beforeEach(() => useProxyStatusMock.mockReset());

  it("starts only the server without taking over CLI configuration", () => {
    const startProxyServer = vi.fn().mockResolvedValue(undefined);
    useProxyStatusMock.mockReturnValue({
      isRunning: false,
      // The disabled takeover query may stay pending; only server status matters.
      isInitialStatusPending: true,
      isStarting: false,
      startProxyServer,
      status: { address: "127.0.0.1", port: 15721 },
    });
    render(<GatewayStatusBar upstream="claude" />);
    fireEvent.click(screen.getByRole("button"));
    expect(startProxyServer).toHaveBeenCalledOnce();
  });

  it("waits for initial status and displays a usable loopback base URL", () => {
    const state = {
      isRunning: false,
      isInitialStatusPending: true,
      isStarting: false,
      status: undefined as { address: string; port: number } | undefined,
    };
    useProxyStatusMock.mockReturnValue(state);
    const { rerender } = render(<GatewayStatusBar upstream="deepseek" />);
    expect(screen.getByRole("button")).toBeDisabled();
    state.isInitialStatusPending = false;
    state.isRunning = true;
    state.status = { address: "0.0.0.0", port: 15721 };
    rerender(<GatewayStatusBar upstream="deepseek" />);
    expect(screen.getByText("http://127.0.0.1:15721/v1")).toBeInTheDocument();
    expect(screen.getByRole("button")).toBeDisabled();
    rerender(<GatewayStatusBar upstream="gemini" />);
    expect(screen.getByText("http://127.0.0.1:15721")).toBeInTheDocument();
  });
});
