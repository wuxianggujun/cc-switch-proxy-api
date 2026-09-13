import { beforeEach, describe, expect, it, vi } from "vitest";
import { checkinApi } from "@/lib/api/checkin";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

describe("isolated check-in browser commands", () => {
  beforeEach(() => invoke.mockReset());

  it("opens the account window using an entry id, not a domain or frontend path", async () => {
    invoke.mockResolvedValue(undefined);
    await checkinApi.openLogin("account-b");
    expect(invoke).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith("open_checkin_login_window", {
      id: "account-b",
    });
  });

  it("loads only account metadata from the backend", async () => {
    const status = { accountCookieCount: 2, loginWindowOpen: false };
    invoke.mockResolvedValue(status);
    expect(await checkinApi.getBrowserSessionStatus("account-b")).toEqual(
      status,
    );
    expect(invoke).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith("get_checkin_browser_session_status", {
      id: "account-b",
    });
  });
});
