/**
 * The fields of one record, each with its label.
 *
 * A generated detail names these and states each term and its text. While the
 * record is read, each value shows a skeleton of its own line.
 */

import { createContext, type JSX, type ParentProps, Show, useContext } from "solid-js";

import { Skeleton } from "./components/ui/skeleton";

const Reading = createContext<() => boolean>(() => false);

export interface DetailListProps {
  /** True while the record is read. */
  readonly loading: boolean;
}

export function DetailList(props: ParentProps<DetailListProps>): JSX.Element {
  return (
    <Reading.Provider value={() => props.loading}>
      <dl class="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">{props.children}</dl>
    </Reading.Provider>
  );
}

export interface DetailItemProps {
  /** The label of the field. */
  readonly term: string;
}

export function DetailItem(props: ParentProps<DetailItemProps>): JSX.Element {
  const reading = useContext(Reading);
  return (
    <>
      <dt class="text-muted-foreground">{props.term}</dt>
      <dd class="min-w-0 break-words">
        <Show when={!reading()} fallback={<Skeleton class="h-4 w-40" />}>
          {props.children}
        </Show>
      </dd>
    </>
  );
}
