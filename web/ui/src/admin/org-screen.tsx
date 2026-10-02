/**
 * The org screen of Control (docs/plan/platform-ui.md §4.4, issue 8).
 *
 * It reads org `user.list` for its members, with one `org-admin` toggle per
 * member that calls `org_admin.grant` or `org_admin.revoke`, and one button
 * that calls `user.activate` or `user.deactivate`. The invite form calls
 * `user.invite` with the memberships, `org-admin` and `project-admin` that
 * the operator chose from `project.list` and `environment.list`.
 *
 * Each write announces its outcome, a refusal shows the text of its contract
 * and the environments a partial write completed, and the screen reads the
 * members again after each write. It writes no
 * database directly.
 */

import { environment, orgAdmin, project, user } from "@wamn/control-org-client";
import { newRequestId, type Outcome, type Transport } from "@wamn/web-runtime";
import { createResource, createSignal, createUniqueId, For, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { Switch } from "../components/ui/switch";
import { CheckField, TextField } from "../fields";
import { announceOutcome } from "../outcome";
import { controlRefusal } from "./refusal";

export interface OrgScreenProps {
  readonly transport: Transport;
}

/** One project of the org and its environments. */
interface Project {
  readonly name: string;
  readonly environments: readonly string[];
}

/** Every project of the org with its environments, in project order. */
async function projects(transport: Transport): Promise<Project[]> {
  const listed = await project.list(transport, [{}]);
  if (listed.status !== "completed") {
    return [];
  }
  const all: Project[] = [];
  for (const name of listed.value.projects) {
    const environments = await environment.list(transport, [{ project: name }]);
    all.push({ name, environments: environments.status === "completed" ? environments.value.environments : [] });
  }
  return all;
}

/** The key of one environment in the invite form. */
const membership = (name: string, env: string) => `${name} ${env}`;

export function OrgScreen(props: OrgScreenProps): JSX.Element {
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const [members, { refetch }] = createResource(async () => {
    const outcome = await user.list(props.transport, [{}]);
    return outcome.status === "completed" ? outcome.value.users : [];
  });
  const [org] = createResource(() => projects(props.transport));

  const [email, setEmail] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [inviteOrgAdmin, setInviteOrgAdmin] = createSignal(false);
  const [projectAdmins, setProjectAdmins] = createSignal<ReadonlySet<string>>(new Set());
  const [memberships, setMemberships] = createSignal<ReadonlySet<string>>(new Set());
  const flip = (set: ReadonlySet<string>, key: string, on: boolean) => {
    const next = new Set(set);
    if (on) {
      next.add(key);
    } else {
      next.delete(key);
    }
    return next;
  };

  /** Announces one write, shows a refusal's text, and reads the members again. */
  const settle = async <T,>(outcome: Outcome<T>, screen: string) => {
    announceOutcome(outcome, screen);
    setRefusal(outcome.status === "refused" ? controlRefusal(outcome) : null);
    await refetch();
    return outcome.status === "completed";
  };

  const toggleOrgAdmin = async (principalId: string, on: boolean) => {
    const item = { requestId: newRequestId(), value: { principalId } };
    if (on) {
      await settle(await orgAdmin.grant(props.transport, [item]), "org-admin grant");
    } else {
      await settle(await orgAdmin.revoke(props.transport, [item]), "org-admin revoke");
    }
  };

  const setActive = async (principalId: string, active: boolean) => {
    const item = { requestId: newRequestId(), value: { principalId } };
    if (active) {
      await settle(await user.activate(props.transport, [item]), "activate");
    } else {
      await settle(await user.deactivate(props.transport, [item]), "deactivate");
    }
  };

  const invite = async () => {
    const chosen = (org() ?? []).flatMap((each) =>
      each.environments
        .filter((env) => memberships().has(membership(each.name, env)))
        .map((env) => ({ project: each.name, env })),
    );
    const value = {
      email: email(),
      displayName: displayName(),
      orgAdmin: inviteOrgAdmin(),
      projectAdmins: [...projectAdmins()].sort(),
      memberships: chosen,
    };
    if (await settle(await user.invite(props.transport, [{ requestId: newRequestId(), value }]), "invite")) {
      setEmail("");
      setDisplayName("");
      setInviteOrgAdmin(false);
      setProjectAdmins(new Set<string>());
      setMemberships(new Set<string>());
    }
  };

  return (
    <div class="flex flex-col gap-6">
      <Show when={refusal()}>{(text) => <p role="alert">{text()}</p>}</Show>
      <section class="flex flex-col gap-2">
        <p class="text-sm font-semibold uppercase">members</p>
        <table class="w-full text-sm" data-slot="org-members">
          <thead>
            <tr class="text-left">
              <th>email</th>
              <th>name</th>
              <th>status</th>
              <th>org-admin</th>
              <th />
            </tr>
          </thead>
          <tbody>
            <For each={members() ?? []}>
              {(member) => {
                const id = createUniqueId();
                const active = () => member.status === "active";
                return (
                  <tr data-member={member.email}>
                    <td>{member.email}</td>
                    <td>{member.displayName}</td>
                    <td>{member.status}</td>
                    <td>
                      <Switch
                        id={id}
                        size="sm"
                        checked={member.orgAdmin}
                        onChange={(on: boolean) => void toggleOrgAdmin(member.principalId, on)}
                      />
                      <label for={id} class="sr-only">
                        org-admin {member.email}
                      </label>
                    </td>
                    <td>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void setActive(member.principalId, !active())}
                      >
                        {active() ? "deactivate" : "activate"}
                      </Button>
                    </td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
      </section>
      <section class="flex max-w-md flex-col gap-3" data-slot="org-invite">
        <p class="text-sm font-semibold uppercase">invite</p>
        <TextField label="email" type="text" value={email()} onInput={setEmail} />
        <TextField label="display name" type="text" value={displayName()} onInput={setDisplayName} />
        <CheckField label="org-admin" checked={inviteOrgAdmin()} onChange={setInviteOrgAdmin} />
        <For each={org() ?? []}>
          {(each) => (
            <div class="flex flex-col gap-2">
              <CheckField
                label={`project-admin ${each.name}`}
                checked={projectAdmins().has(each.name)}
                onChange={(on) => setProjectAdmins(flip(projectAdmins(), each.name, on))}
              />
              <For each={each.environments}>
                {(env) => (
                  <CheckField
                    label={`member ${membership(each.name, env)}`}
                    checked={memberships().has(membership(each.name, env))}
                    onChange={(on) => setMemberships(flip(memberships(), membership(each.name, env), on))}
                  />
                )}
              </For>
            </div>
          )}
        </For>
        <Button class="self-start" onClick={() => void invite()}>
          invite
        </Button>
      </section>
    </div>
  );
}
