/**
 * The administration screens of `@wamn/ui/admin`, over the in-memory
 * application routes of `stubs/admin.ts` and the in-memory Control routes of
 * `stubs/control.ts`. Each screen writes to its own copy.
 */

import type { JSX } from "solid-js";

import { OrgScreen, ProjectScreen, RoleGrid, UserGrid } from "@wamn/ui/admin";

import { BOSS, adminStub } from "../stubs/admin.js";
import { controlStub } from "../stubs/control.js";
import { Section, State } from "./section.js";

export function AdminSections(): JSX.Element {
  return (
    <>
      <Section title="Role grid" name="RoleGrid">
        <State name="clerk, grouped by interface">
          <RoleGrid transport={adminStub().transport} role="clerk" />
        </State>
      </Section>
      <Section title="User grid" name="UserGrid">
        <State name="a user whose admin project-admin covers">
          <UserGrid transport={adminStub().transport} user={BOSS} />
        </State>
      </Section>
      <Section title="Org screen" name="OrgScreen">
        <State name="an org-admin, a project-admin and a member">
          <OrgScreen transport={controlStub().transport} />
        </State>
      </Section>
      <Section title="Project screen" name="ProjectScreen">
        <State name="billing, with a covered org-admin and project-admin">
          <ProjectScreen transport={controlStub().transport} project="billing" />
        </State>
      </Section>
    </>
  );
}
