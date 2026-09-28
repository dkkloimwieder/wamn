/**
 * The group bar of a data table: the grouped columns in nesting order.
 *
 * The operator adds a column, removes one, and moves one up or down a level.
 * Each level chooses how its groups sort, by their value or by one column's
 * aggregate, and expands or collapses all of its groups. A grouped time also
 * chooses its bucket: day, week or month.
 *
 * Grouping runs in the table, which shows the bar only on a fully read set.
 */

import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, X } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { ChoiceField } from "../fields";
import { BUCKETS, type Bucket } from "./aggregate";

/** How the groups of one level sort: by their value, or by one column's aggregate. */
export interface GroupSort {
  /** "value", or the field whose aggregate orders the groups. */
  readonly by: string;
  readonly descending: boolean;
}

export const VALUE_SORT: GroupSort = { by: "value", descending: false };

/** One column the bar can name. */
export interface GroupBarColumn {
  readonly field: string;
  readonly label: string;
  /** True for a timestamptz column, which groups by a bucket. */
  readonly time: boolean;
  /** False for json and bytes, which do not group. */
  readonly groupable: boolean;
  /** The column's chosen aggregate, as a group sort names it. */
  readonly aggregate: string;
}

export function GroupBar(props: {
  columns: readonly GroupBarColumn[];
  /** The grouped fields, in nesting order. */
  grouping: readonly string[];
  bucket: (field: string) => Bucket;
  sort: (field: string) => GroupSort;
  onGrouping: (grouping: readonly string[]) => void;
  onBucket: (field: string, bucket: Bucket) => void;
  onSort: (field: string, sort: GroupSort) => void;
  /** Expand, or collapse, every group of one level. */
  onExpandLevel: (depth: number, expanded: boolean) => void;
}): JSX.Element {
  const column = (field: string) => props.columns.find((candidate) => candidate.field === field)!;
  const move = (index: number, step: -1 | 1) => {
    const next = [...props.grouping];
    [next[index], next[index + step]] = [next[index + step]!, next[index]!];
    props.onGrouping(next);
  };
  return (
    <div data-slot="table-group-bar" class="flex shrink-0 flex-wrap items-end gap-2">
      <For each={props.grouping}>
        {(field, index) => (
          <div
            data-slot="table-group-level"
            class="flex flex-wrap items-end gap-1 border px-2 py-1"
            aria-label={`group level ${index() + 1}`}
            role="group"
          >
            <p class="self-center text-sm font-medium">
              {index() + 1}. {column(field).label}
            </p>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`move ${column(field).label} out`}
              disabled={index() === 0}
              onClick={() => move(index(), -1)}
            >
              <ArrowLeft aria-hidden="true" />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`move ${column(field).label} in`}
              disabled={index() === props.grouping.length - 1}
              onClick={() => move(index(), 1)}
            >
              <ArrowRight aria-hidden="true" />
            </Button>
            <Show when={column(field).time}>
              <For each={BUCKETS}>
                {(bucket) => (
                  <Button
                    type="button"
                    size="xs"
                    variant={props.bucket(field) === bucket ? "default" : "outline"}
                    onClick={() => props.onBucket(field, bucket)}
                  >
                    {bucket}
                  </Button>
                )}
              </For>
            </Show>
            <div class="w-48">
              <ChoiceField
                label={`sort ${column(field).label} groups by`}
                choices={[
                  { value: "value", text: "value" },
                  ...props.columns
                    .filter((candidate) => !props.grouping.includes(candidate.field))
                    .map((candidate) => ({
                      value: candidate.field,
                      text: `${candidate.label} ${candidate.aggregate}`,
                    })),
                ]}
                allowEmpty={false}
                value={props.sort(field).by}
                onChange={(by) => {
                  if (by !== "") {
                    props.onSort(field, { ...props.sort(field), by });
                  }
                }}
              />
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`${column(field).label} groups ${props.sort(field).descending ? "descending" : "ascending"}`}
              onClick={() =>
                props.onSort(field, {
                  ...props.sort(field),
                  descending: !props.sort(field).descending,
                })
              }
            >
              <Show when={props.sort(field).descending} fallback={<ArrowUp aria-hidden="true" />}>
                <ArrowDown aria-hidden="true" />
              </Show>
            </Button>
            <Button type="button" variant="ghost" size="xs" onClick={() => props.onExpandLevel(index(), true)}>
              expand all {column(field).label}
            </Button>
            <Button type="button" variant="ghost" size="xs" onClick={() => props.onExpandLevel(index(), false)}>
              collapse all {column(field).label}
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`remove group ${column(field).label}`}
              onClick={() => props.onGrouping(props.grouping.filter((grouped) => grouped !== field))}
            >
              <X aria-hidden="true" />
            </Button>
          </div>
        )}
      </For>
      <div class="w-48">
        <ChoiceField
          label="group by"
          choices={props.columns
            .filter((candidate) => candidate.groupable && !props.grouping.includes(candidate.field))
            .map((candidate) => ({ value: candidate.field, text: candidate.label }))}
          allowEmpty={false}
          value=""
          onChange={(field) => {
            // The choice clears itself when its options change.
            if (field !== "") {
              props.onGrouping([...props.grouping, field]);
            }
          }}
        />
      </div>
    </div>
  );
}
