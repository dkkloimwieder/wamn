/**
 * The effect profile of a release (docs/plan/trx-pkg-plan.md, Spike and
 * acceptance B), shown as information. Nothing here grants or refuses.
 *
 * `release.profile` answers, for each operation of the release, the
 * operations it reaches and the tables and columns that its statements read,
 * write and lock. `profileOf` folds operations into one summary: one
 * operation for an operation row, and the operations a role holds for the
 * role. A reach that is a base member's whole statement set names every
 * export of the base, each marked imprecise. A statement that the lexer could
 * not read is listed as refused, with the reason.
 */

import { release } from "@wamn/control-org-client";
import type { Transport } from "@wamn/web-runtime";
import { createResource, For, type JSX, Show } from "solid-js";

/** What `release.profile` answers. */
export type ReleaseProfileResult = release.ReleaseProfileResult;

/** The profile of one or more operations, each list sorted. */
export interface ProfileSummary {
  /** The other operations reached, with `(imprecise)` after each one of an imprecise reach. */
  readonly reaches: readonly string[];
  /** `table.column`. */
  readonly reads: readonly string[];
  /** `table.column`, and `table (delete)` for a delete. */
  readonly writes: readonly string[];
  /** The tables locked. */
  readonly locks: readonly string[];
  /** `path: reason` for each statement that the lexer refused. */
  readonly refused: readonly string[];
}

/** The union of the profiles of `operations` in `profile`. An operation the release does not profile adds nothing. */
export function profileOf(profile: ReleaseProfileResult, operations: readonly string[]): ProfileSummary {
  const reaches = new Set<string>();
  const reads = new Set<string>();
  const writes = new Set<string>();
  const locks = new Set<string>();
  const refused = new Set<string>();
  for (const operation of profile.operations.filter((each) => operations.includes(each.operation))) {
    for (const reach of operation.reaches) {
      for (const reached of reach.operations) {
        if (reach.imprecise) {
          reaches.add(`${reached} (imprecise)`);
        } else if (reached !== operation.operation) {
          reaches.add(reached);
        }
      }
      for (const statement of reach.statements) {
        if (statement.refused !== null) {
          refused.add(`${statement.path}: ${statement.refused}`);
        }
        for (const relation of statement.relations) {
          relation.reads.forEach((column) => reads.add(`${relation.table}.${column}`));
          relation.writes.forEach((column) => writes.add(`${relation.table}.${column}`));
          if (relation.deletes) {
            writes.add(`${relation.table} (delete)`);
          }
          if (relation.locks) {
            locks.add(relation.table);
          }
        }
      }
    }
  }
  const sorted = (set: Set<string>) => [...set].sort();
  return {
    reaches: sorted(reaches),
    reads: sorted(reads),
    writes: sorted(writes),
    locks: sorted(locks),
    refused: sorted(refused),
  };
}

/** The fields of a summary, in the order the screens show them. */
export const PROFILE_FIELDS: readonly (keyof ProfileSummary)[] = ["reaches", "reads", "writes", "locks", "refused"];

/** One field of a summary as text. */
export function profileText(items: readonly string[]): string {
  return items.length > 0 ? items.join(", ") : "none";
}

/** A summary as a list of its fields. */
export function ProfileList(props: { summary: ProfileSummary; slot: string }): JSX.Element {
  return (
    <dl class="grid grid-cols-[auto_1fr] gap-x-3 text-sm" data-slot={props.slot}>
      <For each={PROFILE_FIELDS}>
        {(field) => (
          <>
            <dt class="text-muted-foreground">{field}</dt>
            <dd class="break-words">{profileText(props.summary[field])}</dd>
          </>
        )}
      </For>
    </dl>
  );
}

export interface ReleaseProfileProps {
  readonly transport: Transport;
  readonly project: string;
  readonly env: string;
  readonly digest: string;
}

/** The profile of one release of one environment, one row per operation. */
export function ReleaseProfile(props: ReleaseProfileProps): JSX.Element {
  const [profile] = createResource(
    () => ({ project: props.project, env: props.env, digest: props.digest }),
    async (input) => {
      const outcome = await release.profile(props.transport, [input]);
      return outcome.status === "completed" ? outcome.value : null;
    },
  );
  return (
    <Show when={profile()} fallback={<p class="text-muted-foreground text-sm">unavailable</p>}>
      {(current) => (
        <table class="w-full text-sm" data-slot="release-profile">
          <thead>
            <tr class="text-left">
              <th>operation</th>
              <For each={PROFILE_FIELDS}>{(field) => <th>{field}</th>}</For>
            </tr>
          </thead>
          <tbody>
            <For each={current().operations}>
              {(operation) => {
                const summary = profileOf(current(), [operation.operation]);
                return (
                  <tr data-operation={operation.operation}>
                    <td class="font-mono break-all">{operation.operation}</td>
                    <For each={PROFILE_FIELDS}>
                      {(field) => <td class="break-words">{profileText(summary[field])}</td>}
                    </For>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
      )}
    </Show>
  );
}
