import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CheckinStatusBadge } from "@/components/checkin/CheckinStatusBadge";
import type { CheckinResult } from "@/types/checkin";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

describe("check-in authentication status", () => {
  it("shows re-login for failed authentication without adding a fifth result state", () => {
    const result: CheckinResult = {
      status: "failed",
      needsLogin: true,
      at: 1,
      httpStatus: 401,
      message: "Unauthorized",
    };
    render(<CheckinStatusBadge result={result} />);
    expect(screen.getByText("checkin.status.needsLogin")).toBeInTheDocument();
    expect(result.status).toBe("failed");
  });

  it.each(["blocked", "error", "failed", "success"] as const)(
    "preserves the %s result label",
    (status) => {
      render(
        <CheckinStatusBadge result={{ status, at: 1, message: "result" }} />,
      );
      expect(screen.getByText(`checkin.status.${status}`)).toBeInTheDocument();
    },
  );
});
