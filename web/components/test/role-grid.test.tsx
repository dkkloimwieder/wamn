/**
 * The role grid over an in-memory application (docs/plan/platform-ui.md §4.7).
 *
 * `clerk` holds `receiving/record`. The grid shows the direct and the
 * effective state of each operation, and a toggle only where a grant can
 * change.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { operationInterface, RoleGrid } from "@wamn/ui/admin";
import type { JsonValue } from "@wamn/web-runtime";

import { adminState, adminStub } from "../stubs/admin.js";
import { PROFILE } from "../stubs/control.js";
import { pick } from "./choose.js";
import { bodyRows, theButton } from "./dom.js";

afterEach(cleanup);

/**
 * Each data row as its operation, selected, effective and change, in group
 * order. A toggle shows as its hidden label.
 */
const shown = () =>
  bodyRows()
    .filter((row) => row.querySelector('[data-slot="table-group-value"]') === null)
    .map((row) => Array.from(row.querySelectorAll("td")).map((cell) => cell.textContent?.trim() ?? ""))
    .map((cells) => cells.slice(1, 5));

/** The switch of one operation's row. */
const toggle = (operation: string) => screen.getByLabelText(`select ${operation}`) as HTMLInputElement;

async function open(state = adminState(), role = "clerk", profile?: typeof PROFILE) {
  const stub = adminStub(state);
  render(() => <RoleGrid transport={stub.transport} profile={profile} />);
  await pick("role", role);
  await waitFor(() => expect(bodyRows().length).toBeGreaterThan(0));
  fireEvent.click(theButton("expand all interface"));
  return stub;
}

describe("the role grid", () => {
  it("groups by the interface of each reference", () => {
    expect(operationInterface("acme:receiving/record")).toBe("receiving");
    expect(operationInterface("wamn-control:user-role/grant")).toBe("user-role");
    expect(operationInterface("acme:a/b/c")).toBe("a/b");
  });

  it("shows the profile beside each row and the union over the role's operations", async () => {
    await open(adminState(), "clerk", PROFILE);
    const row = (operation: string) =>
      bodyRows()
        .find((each) => each.textContent?.includes(operation))
        ?.textContent?.trim() ?? "";
    expect(row("acme:purchase/get")).toContain("purchase_order.cost, purchase_order.id");
    expect(row("acme:receiving/record")).toContain("base:receipt/void (imprecise)");
    expect(row("acme:receiving/record")).toContain("void/delete.sql: unsupported statement");
    // clerk holds only receiving/record, so the union leaves out purchase_order.cost.
    const role = document.querySelector('[data-slot="role-profile"]')?.textContent ?? "";
    expect(role).toContain("lockspurchase_order");
    expect(role).toContain("readspurchase_order.id, purchase_order.status");
    expect(role).not.toContain("cost");
  });

  it("shows the direct and the effective state of each row", async () => {
    await open();
    expect(shown()).toEqual([
      ["acme:inventory/query", "false", "false", "select acme:inventory/query"],
      ["wamn-control:permission/mine", "false", "false", "fixed"],
      ["acme:purchase/get", "false", "false", "select acme:purchase/get"],
      ["acme:receiving/record", "true", "true", "select acme:receiving/record"],
      ["wamn-control:role/list", "false", "false", "admin only"],
    ]);
    expect(toggle("acme:receiving/record").checked).toBe(true);
    expect(toggle("acme:inventory/query").checked).toBe(false);
    expect(screen.queryByLabelText("select wamn-control:role/list")).toBeNull();
  });

  it("grants an operation and reads the role again", async () => {
    const { sent } = await open();
    fireEvent.click(toggle("acme:inventory/query"));
    await waitFor(() => expect(toggle("acme:inventory/query").checked).toBe(true));
    const grant = sent.find((request) => request.operation.startsWith("wamn-control:permission/grant@"));
    const item = grant?.items[0] as { [key: string]: JsonValue };
    expect(item["value"]).toEqual({ role: "clerk", operation: "acme:inventory/query" });
    expect(shown()[0]).toEqual(["acme:inventory/query", "true", "true", "select acme:inventory/query"]);
  });

  it("shows that admin holds every operation, with no toggle", async () => {
    await open(adminState(), "admin");
    expect(screen.getByText("The role admin holds every operation that the release serves.")).toBeDefined();
    expect(shown().map((cells) => cells[2])).toEqual(["true", "true", "true", "true", "true"]);
    expect(shown().map((cells) => cells[3])).toEqual(["admin", "admin", "admin", "admin", "admin"]);
  });

  it("shows a refusal with its contract text, and the state it read again", async () => {
    const state = adminState();
    const { sent } = await open(state);
    state.refuseNext = "operation_not_served";
    fireEvent.click(toggle("acme:inventory/query"));
    expect(await screen.findByText("The current release does not serve this operation.")).toBeDefined();
    expect(toggle("acme:inventory/query").checked).toBe(false);
    expect(sent.filter((request) => request.operation.startsWith("wamn-control:permission/list@"))).toHaveLength(2);
  });
});
