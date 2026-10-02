/**
 * The role grid over an in-memory application (docs/plan/platform-ui.md §4.7).
 *
 * `clerk` selected `receiving/record`, whose closure also holds
 * `purchase/get`. The grid shows the direct and the effective state of each
 * operation, the roots that require it, and a toggle only where a direct
 * selection can change.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { operationInterface, RoleGrid } from "@wamn/ui/admin";
import type { JsonValue } from "@wamn/web-runtime";

import { adminState, adminStub } from "../stubs/admin.js";
import { pick } from "./choose.js";
import { bodyRows, theButton } from "./dom.js";

afterEach(cleanup);

/**
 * Each data row as its operation, selected, effective, required by and
 * change, in group order. A toggle shows as its hidden label.
 */
const shown = () =>
  bodyRows()
    .filter((row) => row.querySelector('[data-slot="table-group-value"]') === null)
    .map((row) => Array.from(row.querySelectorAll("td")).map((cell) => cell.textContent?.trim() ?? ""))
    .map((cells) => cells.slice(1, 6));

/** The switch of one operation's row. */
const toggle = (operation: string) => screen.getByLabelText(`select ${operation}`) as HTMLInputElement;

async function open(state = adminState(), role = "clerk") {
  const stub = adminStub(state);
  render(() => <RoleGrid transport={stub.transport} />);
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

  it("shows the direct and the effective state, and the roots of each required row", async () => {
    await open();
    expect(shown()).toEqual([
      ["acme:inventory/query", "false", "false", "", "select acme:inventory/query"],
      ["wamn-control:permission/mine", "false", "false", "", "fixed"],
      ["acme:purchase/get", "false", "true", "acme:receiving/record", "required"],
      ["acme:receiving/record", "true", "true", "", "select acme:receiving/record"],
      ["wamn-control:role/list", "false", "false", "", "admin only"],
    ]);
    expect(toggle("acme:receiving/record").checked).toBe(true);
    expect(toggle("acme:inventory/query").checked).toBe(false);
    expect(screen.queryByLabelText("select acme:purchase/get")).toBeNull();
    expect(screen.queryByLabelText("select wamn-control:role/list")).toBeNull();
  });

  it("grants a root and reads its closure again", async () => {
    const { sent } = await open();
    fireEvent.click(toggle("acme:inventory/query"));
    await waitFor(() => expect(toggle("acme:inventory/query").checked).toBe(true));
    const grant = sent.find((request) => request.operation.startsWith("wamn-control:permission/grant@"));
    const item = grant?.items[0] as { [key: string]: JsonValue };
    expect(item["value"]).toEqual({ role: "clerk", operation: "acme:inventory/query" });
    expect(shown()[0]).toEqual(["acme:inventory/query", "true", "true", "", "select acme:inventory/query"]);
  });

  it("keeps a revoked row that another root requires, and names that root", async () => {
    const state = adminState();
    state.roots.get("clerk")?.add("acme:purchase/get");
    await open(state);
    expect(shown()[2]).toEqual([
      "acme:purchase/get",
      "true",
      "true",
      "acme:receiving/record",
      "select acme:purchase/get",
    ]);
    fireEvent.click(toggle("acme:purchase/get"));
    await waitFor(() =>
      expect(shown()[2]).toEqual(["acme:purchase/get", "false", "true", "acme:receiving/record", "required"]),
    );
    expect(screen.queryByLabelText("select acme:purchase/get")).toBeNull();
  });

  it("shows that admin holds every operation, with no toggle", async () => {
    await open(adminState(), "admin");
    expect(screen.getByText("The role admin holds every operation that the release serves.")).toBeDefined();
    expect(shown().map((cells) => cells[2])).toEqual(["true", "true", "true", "true", "true"]);
    expect(shown().map((cells) => cells[4])).toEqual(["admin", "admin", "admin", "admin", "admin"]);
  });

  it("shows a refusal with its contract text, and the state it read again", async () => {
    const state = adminState();
    const { sent } = await open(state);
    state.refuseNext = "release_not_current";
    fireEvent.click(toggle("acme:inventory/query"));
    expect(
      await screen.findByText(
        "The release of this host is no longer the current release. Try again when the host serves the current release.",
      ),
    ).toBeDefined();
    expect(toggle("acme:inventory/query").checked).toBe(false);
    expect(sent.filter((request) => request.operation.startsWith("wamn-control:permission/list@"))).toHaveLength(2);
  });
});
