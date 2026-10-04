/**
 * The project screen over an in-memory org (docs/plan/platform-ui.md §4.5).
 *
 * In billing, Boss holds `org-admin`, Cat `project-admin`, and Ann only the
 * membership of `dev`. In shop, only Boss is listed.
 */

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
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

  it("shows each environment saga with its steps and the operator commands", async () => {
    const state = controlState();
    const step = (number: number, name: string, status: string, error: string | null, detail: JsonValue) => ({
      step: number,
      name,
      status,
      error,
      detail,
      started_at: null,
      finished_at: null,
    });
    state.sagas["billing"] = [
      {
        saga_id: "saga-test",
        env: "test",
        status: "failed",
        last_error: "the run plane refused",
        steps: [
          step(1, "provision-project-env", "completed", null, null),
          step(2, "reconcile-run-plane", "failed", "the run plane refused", null),
        ],
      },
      {
        saga_id: "saga-stage",
        env: "stage",
        status: "awaiting-operator",
        last_error: null,
        steps: [
          step(15, "awaiting-operator", "completed", null, {
            commands: [
              {
                purpose: "the identity restart",
                runbook: "docs/operations/gcp.md section 3.8",
                commands: ["kubectl -n identity rollout restart deploy/identity"],
              },
            ],
          }),
        ],
      },
    ];
    await open("billing", state);
    const failed = document.querySelector('[data-saga="saga-test"]');
    expect(failed?.textContent).toContain("test: failed, the run plane refused");
    expect(failed?.querySelector('[data-step="reconcile-run-plane"]')?.textContent).toBe(
      "2reconcile-run-planefailedthe run plane refused",
    );
    const waiting = document.querySelector('[data-saga="saga-stage"]');
    expect(waiting?.textContent).toContain("stage: awaiting-operator");
    expect(waiting?.querySelector('[data-slot="operator-commands"]')?.textContent).toBe(
      "the identity restart (docs/operations/gcp.md section 3.8)kubectl -n identity rollout restart deploy/identity",
    );
  });

  it("resumes a failed saga, and abandons a pending one once the operator confirms", async () => {
    const state = controlState();
    const saga = (sagaId: string, env: string, status: string) => ({
      saga_id: sagaId,
      env,
      status,
      last_error: status === "failed" ? "the run plane refused" : null,
      steps: [],
    });
    state.sagas["billing"] = [saga("saga-test", "test", "failed"), saga("saga-stage", "stage", "awaiting-operator")];
    const { sent } = await open("billing", state);
    const buttons = (sagaId: string) =>
      Array.from(document.querySelectorAll(`[data-saga="${sagaId}"] button`), (button) => button.textContent);
    expect(buttons("saga-test")).toEqual(["resume", "abandon"]);
    expect(buttons("saga-stage")).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "resume" }));
    await waitFor(() =>
      expect(document.querySelector('[data-saga="saga-test"]')?.textContent).toContain("test: pending"),
    );
    expect(written(sent, "environment/resume")).toEqual({ saga_id: "saga-test" });
    expect(buttons("saga-test")).toEqual(["abandon"]);

    fireEvent.click(screen.getByRole("button", { name: "abandon" }));
    await waitFor(() => expect(screen.getByRole("alertdialog")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: "cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(written(sent, "environment/abandon")).toBeUndefined();

    fireEvent.click(screen.getByRole("button", { name: "abandon" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog.textContent).toContain("Abandon the creation of test?");
    fireEvent.click(within(dialog).getByRole("button", { name: "abandon" }));
    await waitFor(() =>
      expect(document.querySelector('[data-saga="saga-test"]')?.textContent).toContain("test: abandoned"),
    );
    expect(written(sent, "environment/abandon")).toEqual({ saga_id: "saga-test" });
    expect(buttons("saga-test")).toEqual([]);
  });

  it("shows a refused resume with its contract text", async () => {
    const state = controlState();
    state.sagas["billing"] = [{ saga_id: "saga-test", env: "test", status: "failed", last_error: null, steps: [] }];
    await open("billing", state);
    state.refuseNext = { code: "saga_not_resumable", detail: { field: "saga_id" } };
    fireEvent.click(screen.getByRole("button", { name: "resume" }));
    expect(await screen.findByText("Only a failed saga resumes.")).toBeDefined();
  });

  it("creates an environment from the pushed package versions and one JSON object per connection", async () => {
    const { sent } = await open("billing");
    const form = await waitFor(() => {
      const found = document.querySelector('[data-slot="project-create-environment"]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    const type = (label: string, text: string, scope: HTMLElement = form) =>
      fireEvent.input(within(scope).getByLabelText(label), { target: { value: text } });
    type("environment name", "test");
    type("tenant", "billing-test");
    type("route host", "billing.example.test");
    await pick("version of wamn_receiving", "1.1.0");
    fireEvent.click(within(form).getByRole("button", { name: "add connection" }));
    const connection = await waitFor(() => form.querySelector("[data-connection]") as HTMLElement);
    type("instance id", "labels", connection);
    type("alias", "labels", connection);
    type("definition", '{"provider": "gcs", "container": "c", "prefix": "p"}', connection);
    fireEvent.click(within(form).getByRole("button", { name: "create" }));
    await waitFor(() =>
      expect(document.querySelector('[data-saga="saga-test"]')?.textContent).toContain("test: pending"),
    );
    expect(written(sent, "environment/create")).toEqual({
      project: "billing",
      env: "test",
      tenant: "billing-test",
      route_host: "billing.example.test",
      packages: [{ package_id: "wamn_receiving", version: "1.1.0" }],
      connections: [
        {
          instance_id: "labels",
          alias: "labels",
          requirement_type: "blobstore",
          definition: { provider: "gcs", container: "c", prefix: "p" },
        },
      ],
    });
  });

  it("refuses a definition that is not a JSON object before it sends anything", async () => {
    const { sent } = await open("billing");
    const form = await waitFor(() => document.querySelector('[data-slot="project-create-environment"]') as HTMLElement);
    fireEvent.click(within(form).getByRole("button", { name: "add connection" }));
    const connection = await waitFor(() => form.querySelector("[data-connection]") as HTMLElement);
    fireEvent.input(within(connection).getByLabelText("instance id"), { target: { value: "labels" } });
    fireEvent.input(within(connection).getByLabelText("definition"), { target: { value: "[1]" } });
    fireEvent.click(within(form).getByRole("button", { name: "create" }));
    expect(await screen.findByText("The definition of labels is not a JSON object.")).toBeDefined();
    expect(written(sent, "environment/create")).toBeUndefined();
  });

  it("shows the reason a definition is refused", async () => {
    const state = controlState();
    await open("billing", state);
    const form = await waitFor(() => document.querySelector('[data-slot="project-create-environment"]') as HTMLElement);
    state.refuseNext = {
      code: "invalid_input",
      detail: {
        field: "connections",
        reason: "the credential handle must not be empty; the host resolves it by name",
      },
    };
    fireEvent.click(within(form).getByRole("button", { name: "create" }));
    expect(
      await screen.findByText(/the credential handle must not be empty; the host resolves it by name$/),
    ).toBeDefined();
  });

  it("copies an environment of the project with one replacement row per connection", async () => {
    const { sent } = await open("billing");
    const form = await waitFor(() => {
      const found = document.querySelector('[data-slot="project-copy-environment"]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });
    const type = (label: string, text: string, scope: HTMLElement = form) =>
      fireEvent.input(within(scope).getByLabelText(label), { target: { value: text } });
    await pick("source environment", "dev");
    type("new environment name", "test");
    type("new tenant", "billing-test");
    type("new route host", "billing.example.test");
    fireEvent.click(within(form).getByRole("button", { name: "add replacement" }));
    const replacement = await waitFor(() => form.querySelector("[data-replacement]") as HTMLElement);
    type("instance id", "labels", replacement);
    type("definition", '{"provider": "gcs", "container": "c", "prefix": "p"}', replacement);
    fireEvent.click(within(form).getByRole("button", { name: "copy" }));
    await waitFor(() =>
      expect(document.querySelector('[data-saga="saga-test"]')?.textContent).toContain("test (copy of dev): pending"),
    );
    expect(written(sent, "environment/copy")).toEqual({
      project: "billing",
      source_env: "dev",
      env: "test",
      tenant: "billing-test",
      route_host: "billing.example.test",
      connections: [{ instance_id: "labels", definition: { provider: "gcs", container: "c", prefix: "p" } }],
    });
  });

  it("refuses a replacement that is not a JSON object before it sends anything", async () => {
    const { sent } = await open("billing");
    const form = await waitFor(() => document.querySelector('[data-slot="project-copy-environment"]') as HTMLElement);
    await pick("source environment", "dev");
    fireEvent.click(within(form).getByRole("button", { name: "add replacement" }));
    const replacement = await waitFor(() => form.querySelector("[data-replacement]") as HTMLElement);
    fireEvent.input(within(replacement).getByLabelText("instance id"), { target: { value: "labels" } });
    fireEvent.input(within(replacement).getByLabelText("definition"), { target: { value: "[1]" } });
    fireEvent.click(within(form).getByRole("button", { name: "copy" }));
    expect(await screen.findByText("The definition of labels is not a JSON object.")).toBeDefined();
    expect(written(sent, "environment/copy")).toBeUndefined();
  });

  it("shows no create or copy form to a project admin", async () => {
    const state = controlState();
    state.callerOrgAdmin = false;
    const { sent } = await open("billing", state);
    await waitFor(() =>
      expect(sent.some((each) => each.operation.startsWith("wamn-control:control/mine@"))).toBe(true),
    );
    expect(document.querySelector('[data-slot="project-create-environment"]')).toBeNull();
    expect(document.querySelector('[data-slot="project-copy-environment"]')).toBeNull();
  });

  it("says when the project has no environment creation", async () => {
    await open("shop");
    expect(screen.getByText("No environment creation.")).toBeDefined();
  });
});
