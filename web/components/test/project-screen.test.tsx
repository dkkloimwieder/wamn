/**
 * The project screen over an in-memory org (docs/plan/platform-ui.md §4.5).
 *
 * In billing, Boss holds `org-admin`, Cat `project-admin`, and Ann only the
 * membership of `dev`. In shop, only Boss is listed.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { ProjectScreen } from "@wamn/ui/admin";
import type { JsonValue } from "@wamn/web-runtime";

import { ANN, CAT, controlState, controlStub } from "../stubs/control.js";
import { pick } from "./choose.js";

afterEach(cleanup);

/** The switch whose screen reader label is `label`. */
const toggle = (label: string) => screen.queryByLabelText(label) as HTMLInputElement | null;

/** The row of one member. */
const row = (email: string) => document.querySelector(`[data-member="${email}"]`);

/** The value of the last write to `route`. */
function written(sent: { operation: string; items: readonly JsonValue[] }[], route: string): JsonValue | undefined {
  const request = sent.filter((each) => each.operation.startsWith(`wamn-control:${route}@`)).at(-1);
  return (request?.items[0] as { [key: string]: JsonValue } | undefined)?.["value"];
}

async function open(project: string, state = controlState()) {
  const stub = controlStub(state);
  render(() => <ProjectScreen transport={stub.transport} project={project} />);
  await waitFor(() => expect(row("boss@example.test")).not.toBeNull());
  return stub;
}

describe("the project screen", () => {
  it("shows each member's environments, and a covered grant as hierarchy-controlled", async () => {
    await open("billing");
    expect(toggle("dev ann@example.test")?.checked).toBe(true);
    expect(toggle("project-admin ann@example.test")?.checked).toBe(false);
    expect(toggle("project-admin cat@example.test")?.checked).toBe(true);
    // Cat's project-admin covers its membership, and Boss's org-admin covers both.
    expect(toggle("dev cat@example.test")).toBeNull();
    expect(toggle("dev boss@example.test")).toBeNull();
    expect(toggle("project-admin boss@example.test")).toBeNull();
    expect(row("boss@example.test")?.textContent).toContain("hierarchy-controlled");
  });

  it("grants and revokes project-admin", async () => {
    const { sent } = await open("billing");
    fireEvent.click(toggle("project-admin ann@example.test")!);
    await waitFor(() => expect(toggle("project-admin ann@example.test")?.checked).toBe(true));
    expect(written(sent, "project-admin/grant")).toEqual({ project: "billing", principal_id: ANN });
    fireEvent.click(toggle("project-admin cat@example.test")!);
    await waitFor(() => expect(toggle("project-admin cat@example.test")?.checked).toBe(false));
    expect(toggle("dev cat@example.test")?.checked).toBe(true);
  });

  it("revokes a membership, and the member leaves the project", async () => {
    const { sent } = await open("billing");
    fireEvent.click(toggle("dev ann@example.test")!);
    await waitFor(() => expect(row("ann@example.test")).toBeNull());
    expect(written(sent, "member/revoke")).toEqual({ project: "billing", env: "dev", principal_id: ANN });
  });

  it("adds a member of the org to an environment of the project", async () => {
    const { sent } = await open("shop");
    expect(row("cat@example.test")).toBeNull();
    await pick("user", "cat@example.test");
    await pick("environment", "dev");
    fireEvent.click(screen.getByRole("button", { name: "add" }));
    await waitFor(() => expect(toggle("dev cat@example.test")?.checked).toBe(true));
    expect(written(sent, "member/grant")).toEqual({ project: "shop", env: "dev", principal_id: CAT });
  });

  it("shows a refusal with its contract text", async () => {
    const state = controlState();
    await open("billing", state);
    state.refuseNext = { code: "user_not_active", detail: { field: "principal_id" } };
    fireEvent.click(toggle("project-admin ann@example.test")!);
    expect(await screen.findByText("The user is not an active member of the org.")).toBeDefined();
    expect(toggle("project-admin ann@example.test")?.checked).toBe(false);
  });
});
