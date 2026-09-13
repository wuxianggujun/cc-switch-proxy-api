import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CheckinSiteFormModal } from "@/components/checkin/CheckinSiteFormModal";
import { emptyCheckinSite, type CheckinSite } from "@/types/checkin";

const actions = vi.hoisted(() => ({
  openLogin: vi.fn(),
  refreshClearance: vi.fn(),
  refreshSession: vi.fn(),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/hooks/useCheckin", () => ({
  useRefreshCheckinClearance: () => ({
    mutate: actions.refreshClearance,
    isPending: false,
  }),
  useOpenCheckinLogin: () => ({
    mutate: actions.openLogin,
    isPending: false,
  }),
  useCheckinBrowserSession: () => ({
    data: { accountCookieCount: 2, loginWindowOpen: false },
    isLoading: false,
    isFetching: false,
    isError: false,
    refetch: actions.refreshSession,
  }),
}));

function browserSite(id = "account-b"): CheckinSite {
  const site = emptyCheckinSite();
  return {
    ...site,
    id,
    name: "Account B",
    siteUrl: "https://same.example",
    authKind: "browser",
    browser: { challengeUrl: "" },
    request: { ...site.request, url: "https://same.example/api/checkin" },
  };
}

function show(site: CheckinSite, onSave = vi.fn()) {
  render(
    <CheckinSiteFormModal
      open
      site={site}
      onSave={onSave}
      onCancel={vi.fn()}
    />,
  );
  return onSave;
}

describe("CheckinSiteFormModal isolated browser profiles", () => {
  beforeEach(() => vi.clearAllMocks());

  it("opens the login window for the saved entry, not the shared domain", () => {
    show(browserSite());
    fireEvent.click(
      screen.getByRole("button", { name: "checkin.form.openLogin" }),
    );
    expect(actions.openLogin).toHaveBeenCalledWith("account-b");
  });

  it("requires a saved entry before opening a persistent profile", () => {
    show(browserSite(""));
    expect(
      screen.getByRole("button", { name: "checkin.form.openLogin" }),
    ).toBeDisabled();
    expect(actions.openLogin).not.toHaveBeenCalled();
  });

  it("does not operate on stale saved URLs while the form contains edits", () => {
    show(browserSite());
    fireEvent.change(screen.getByLabelText("checkin.form.url"), {
      target: { value: "https://other.example/api/checkin" },
    });
    expect(
      screen.getByRole("button", { name: "checkin.form.openLogin" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "checkin.form.refreshClearance" }),
    ).toBeDisabled();
  });

  it("saves the configured login page without copying browser credentials", () => {
    const onSave = show(browserSite());
    fireEvent.change(screen.getByLabelText("checkin.form.browserLoginUrl"), {
      target: { value: "https://same.example/login" },
    });
    fireEvent.click(screen.getByRole("button", { name: "common.save" }));
    expect(onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "account-b",
        browser: { challengeUrl: "", loginUrl: "https://same.example/login" },
      }),
    );
  });
});
