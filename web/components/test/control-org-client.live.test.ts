// @vitest-environment node
/**
 * The org and project routes of the control serving root through their
 * generated client (docs/plan/platform-ui.md §4.4 and §4.5).
 *
 * `control_client_live` in `tests/integration` serves the routes over HTTP
 * and runs this file with the endpoint, two session tokens and the user ids.
 * Without them it skips.
 */
import { describe, expect, test } from "vitest";

import { createTransport } from "@wamn/web-runtime";
import { control, member, orgAdmin, project, projectAdmin, user } from "@wamn/control-org-client";

// The package has no Node types, and the test reads only its environment.
declare const process: { readonly env: { readonly [name: string]: string | undefined } };

const env = (name: string) => process.env[`WAMN_CONTROL_ORG_CLIENT_${name}`];
const baseUrl = env("BASE_URL");
const boss = env("BOSS_TOKEN");
const cat = env("CAT_TOKEN");
const bossId = env("BOSS_ID") ?? "";
const catId = env("CAT_ID") ?? "";
const annId = env("ANN_ID") ?? "";
const danId = env("DAN_ID") ?? "";

describe.skipIf(baseUrl === undefined)("the generated control org client", () => {
  const as = (credential: string | undefined) => createTransport({ baseUrl: baseUrl ?? "", credential });
  let request = 0;
  const requestId = () => `control-org-client-${(request += 1)}`;

  test("admits a project admin to its project only", async () => {
    const transport = as(cat);
    expect(await control.mine(transport, [{}])).toEqual({
      status: "completed",
      value: { orgAdmin: false, projects: [{ project: "billing", projectAdmin: true }] },
    });
    expect(await user.list(transport, [{}])).toMatchObject({
      status: "completed",
      value: { users: [{ principalId: annId }, { principalId: bossId }, { principalId: catId }] },
    });
    expect(await project.list(transport, [{}])).toMatchObject({ status: "refused", code: "permission-denied" });
    expect(await member.list(transport, [{ project: "billing" }])).toMatchObject({
      status: "completed",
      value: {
        members: [
          { principalId: bossId, orgAdmin: true, projectAdmin: true, environments: ["dev"] },
          { principalId: catId, orgAdmin: false, projectAdmin: true, environments: ["dev"] },
        ],
      },
    });
  });

  test("answers and refuses as the contracts declare", async () => {
    const transport = as(boss);
    expect(await user.list(transport, [{}])).toEqual({
      status: "completed",
      value: {
        users: [
          { principalId: annId, email: "ann@example.test", displayName: "Ann", status: "active", orgAdmin: false },
          { principalId: bossId, email: "boss@example.test", displayName: "Boss", status: "active", orgAdmin: true },
          { principalId: catId, email: "cat@example.test", displayName: "Cat", status: "active", orgAdmin: false },
        ],
      },
    });
    expect(await project.list(transport, [{}])).toEqual({
      status: "completed",
      value: { projects: ["billing", "shop"] },
    });
    expect(
      await user.activate(transport, [{ requestId: requestId(), value: { principalId: annId } }]),
    ).toEqual({ status: "completed", value: { principalId: annId, status: "active" } });
    expect(
      await user.activate(transport, [{ requestId: requestId(), value: { principalId: danId } }]),
    ).toMatchObject({ status: "refused", code: "user_not_found", detail: { detail: { field: "principal_id" } } });
    expect(
      await orgAdmin.grant(transport, [{ requestId: requestId(), value: { principalId: danId } }]),
    ).toMatchObject({ status: "refused", code: "user_not_active", detail: { detail: { field: "principal_id" } } });
    expect(
      await member.grant(transport, [
        { requestId: requestId(), value: { project: "billing", env: "prod", principalId: annId } },
      ]),
    ).toMatchObject({ status: "refused", code: "environment_not_found", detail: { detail: { field: "env" } } });
    expect(
      await projectAdmin.grant(transport, [
        { requestId: requestId(), value: { project: "nowhere", principalId: annId } },
      ]),
    ).toMatchObject({ status: "refused", code: "project_not_found", detail: { detail: { field: "project" } } });
    expect(
      await projectAdmin.revoke(transport, [
        { requestId: requestId(), value: { project: "billing", principalId: bossId } },
      ]),
    ).toMatchObject({ status: "refused", code: "admin_covered", detail: { detail: { field: "principal_id" } } });
  });
});
