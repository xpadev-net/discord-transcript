import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SummaryPanel } from "./SummaryPanel";

function renderSummary(markdown: string) {
  return render(
    <SummaryPanel
      markdown={markdown}
      loading={false}
      error={null}
      onRetry={vi.fn()}
    />,
  );
}

afterEach(() => {
  cleanup();
});

describe("SummaryPanel markdown rendering", () => {
  it("renders image markdown without creating an external image request", () => {
    renderSummary("![tracking pixel](https://tracker.example/pixel.png)");

    expect(document.querySelector("img")).toBeNull();
    expect(screen.getByText("tracking pixel")).toBeTruthy();
  });

  it("renders external links as non-clickable text", () => {
    renderSummary("[external reference](https://attacker.example/landing)");

    expect(
      screen.queryByRole("link", { name: "external reference" }),
    ).toBeNull();
    expect(screen.getByText("external reference")).toBeTruthy();
  });

  it("renders hash and same-origin links as safe links", () => {
    renderSummary("[section](#decisions) [meeting](/meetings/meeting-1)");

    expect(screen.getByRole("link", { name: "section" })).toHaveProperty(
      "hash",
      "#decisions",
    );
    expect(screen.getByRole("link", { name: "meeting" })).toHaveProperty(
      "pathname",
      "/meetings/meeting-1",
    );
  });

  it("continues to render normal summary markdown text", () => {
    renderSummary(
      "## Decisions\n\n- Ship the safe renderer\n\n**Owner:** team",
    );

    expect(
      screen.getByRole("heading", { name: "Decisions", level: 2 }),
    ).toBeTruthy();
    expect(screen.getByText("Ship the safe renderer")).toBeTruthy();
    expect(screen.getByText("Owner:")).toBeTruthy();
  });
});
