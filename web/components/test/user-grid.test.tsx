/**
 * The user grid over an in-memory application (docs/plan/platform-ui.md §4.7).
 *
 * ann holds `clerk`. boss holds `admin`, which `project-admin` covers, so
 * `user.list` answers `admin_covered` and the `admin` row has no toggle.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { UserGrid } from "@wamn/ui/admin";
import type { JsonValue } from "@wamn/web-runtime";

import { ANN, adminState, adminStub } from "../stubs/admin.js";
import { pick } from "./choose.js";

afterEach(cleanup);

/** The effective permissions as the grid lists them. */
const effective = () =>
  Array.from(document.querySelectorAll('[data-slot="effective-permissions"] li')).map((item) => item.textContent);

/** The switch of one role. */
const roleSwitch = (role: string) => screen.queryByLabelText(role) as HTMLInputElement | null;

async function open(email: string, state = adminState()) {
  const stub = adminStub(state);
  render(() => <UserGrid transport={stub.transport} />);
  await pick("user", email);
  await screen.findByText("effective permissions");
  return stub;
}

describe("the user grid", () => {
  it("shows the roles the user holds and the union of their permissions", async () => {
    await open("ann@example.test");
    expect(roleSwitch("clerk")?.checked).toBe(true);
    expect(roleSwitch("admin")?.checked).toBe(false);
    await waitFor(() => expect(effective()).toEqual(["acme:purchase/get", "acme:receiving/record"]));
  });

  it("grants and revokes a role, and reads the user again", async () => {
    const { sent } = await open("ann@example.test");
    fireEvent.click(roleSwitch("admin")!);
    await waitFor(() => expect(roleSwitch("admin")?.checked).toBe(true));
    const grant = sent.find((request) => request.operation.startsWith("wamn-control:user-role/grant@"));
    expect((grant?.items[0] as { [key: string]: JsonValue })["value"]).toEqual({ user_id: ANN, role: "admin" });
    await waitFor(() => expect(effective()).toHaveLength(5));

    fireEvent.click(roleSwitch("clerk")!);
    await waitFor(() => expect(roleSwitch("clerk")?.checked).toBe(false));
    fireEvent.click(roleSwitch("admin")!);
    await waitFor(() => expect(roleSwitch("admin")?.checked).toBe(false));
    await waitFor(() => expect(effective()).toEqual([]));
  });

  it("shows a covered admin as hierarchy-controlled, with no toggle", async () => {
    await open("boss@example.test");
    expect(roleSwitch("admin")).toBeNull();
    expect(screen.getByText("hierarchy-controlled")).toBeDefined();
    expect(roleSwitch("clerk")?.checked).toBe(false);
    await waitFor(() => expect(effective()).toHaveLength(5));
  });

  it("shows a refusal with its contract text", async () => {
    const state = adminState();
    await open("ann@example.test", state);
    state.refuseNext = "user_not_found";
    fireEvent.click(roleSwitch("admin")!);
    expect(await screen.findByText("The application has no such user.")).toBeDefined();
    expect(roleSwitch("admin")?.checked).toBe(false);
  });
});
