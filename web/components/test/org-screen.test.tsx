/**
 * The org screen over an in-memory org (docs/plan/platform-ui.md §4.4).
 *
 * Boss holds `org-admin`, Cat `project-admin` of billing, and Ann only a
 * membership. The invite form is tested here with the stub, because a real
 * invite needs the identity service.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { OrgScreen } from "@wamn/ui/admin";
import type { JsonValue } from "@wamn/web-runtime";

import { ANN, controlState, controlStub } from "../stubs/control.js";

afterEach(cleanup);

/** The `org-admin` switch of one member. */
const orgAdminSwitch = (email: string) => screen.queryByLabelText(`org-admin ${email}`) as HTMLInputElement | null;

/** The row of one member. */
const row = (email: string) => document.querySelector(`[data-member="${email}"]`);

/** The value of the last write to `route`. */
function written(sent: { operation: string; items: readonly JsonValue[] }[], route: string): JsonValue | undefined {
  const request = sent.filter((each) => each.operation.startsWith(`wamn-control:${route}@`)).at(-1);
  return (request?.items[0] as { [key: string]: JsonValue } | undefined)?.["value"];
}

async function open(state = controlState()) {
  const stub = controlStub(state);
  render(() => <OrgScreen transport={stub.transport} />);
  await waitFor(() => expect(row("ann@example.test")).not.toBeNull());
  return stub;
}

describe("the org screen", () => {
  it("lists the members with their status and org-admin", async () => {
    await open();
    expect(orgAdminSwitch("boss@example.test")?.checked).toBe(true);
    expect(orgAdminSwitch("ann@example.test")?.checked).toBe(false);
    expect(row("ann@example.test")?.textContent).toContain("active");
  });

  it("grants and revokes org-admin, and reads the members again", async () => {
    const { sent } = await open();
    fireEvent.click(orgAdminSwitch("ann@example.test")!);
    await waitFor(() => expect(orgAdminSwitch("ann@example.test")?.checked).toBe(true));
    expect(written(sent, "org-admin/grant")).toEqual({ principal_id: ANN });
    fireEvent.click(orgAdminSwitch("ann@example.test")!);
    await waitFor(() => expect(orgAdminSwitch("ann@example.test")?.checked).toBe(false));
    expect(written(sent, "org-admin/revoke")).toEqual({ principal_id: ANN });
  });

  it("deactivates and activates a member", async () => {
    await open();
    fireEvent.click(row("ann@example.test")!.querySelector("button")!);
    await waitFor(() => expect(row("ann@example.test")?.textContent).toContain("inactive"));
    fireEvent.click(row("ann@example.test")!.querySelector("button")!);
    await waitFor(() => expect(row("ann@example.test")?.textContent).not.toContain("inactive"));
  });

  it("invites a user with the memberships and grants the form chose", async () => {
    const { sent } = await open();
    await screen.findByLabelText("member billing dev");
    fireEvent.input(screen.getByLabelText("email"), { target: { value: "dan@example.test" } });
    fireEvent.input(screen.getByLabelText("display name"), { target: { value: "Dan" } });
    fireEvent.click(screen.getByLabelText("member billing dev"));
    fireEvent.click(screen.getByLabelText("project-admin shop"));
    fireEvent.click(screen.getByRole("button", { name: "invite" }));
    await waitFor(() => expect(row("dan@example.test")).not.toBeNull());
    expect(written(sent, "user/invite")).toEqual({
      email: "dan@example.test",
      display_name: "Dan",
      org_admin: false,
      project_admins: ["shop"],
      memberships: [{ project: "billing", env: "dev" }],
    });
  });

  it("shows a refusal with its contract text, and the environments a partial write completed", async () => {
    const state = controlState();
    await open(state);
    state.refuseNext = { code: "user_not_active", detail: { field: "principal_id" } };
    fireEvent.click(orgAdminSwitch("ann@example.test")!);
    expect(await screen.findByText("The user is not an active member of the org.")).toBeDefined();
    expect(orgAdminSwitch("ann@example.test")?.checked).toBe(false);

    state.refuseNext = {
      code: "application_write_incomplete",
      detail: { environment: "shop/dev", completed: ["billing/dev"] },
    };
    fireEvent.click(orgAdminSwitch("ann@example.test")!);
    expect(
      await screen.findByText(
        "The application rows stopped at this environment. The completed environments are done. Stopped at shop/dev. Completed: billing/dev.",
      ),
    ).toBeDefined();
  });
});
