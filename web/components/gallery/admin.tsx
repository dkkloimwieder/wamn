/**
 * The administration screens of `@wamn/ui/admin`, over the in-memory
 * application routes of `stubs/admin.ts`. Each grid writes to its own copy.
 */

import type { JSX } from "solid-js";

import { RoleGrid, UserGrid } from "@wamn/ui/admin";

import { BOSS, adminStub } from "../stubs/admin.js";
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
    </>
  );
}
