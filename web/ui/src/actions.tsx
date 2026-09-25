/**
 * The layout a generated screen names, so that it states no class itself.
 *
 * FormActions closes a form or a table with its buttons. The row spans the
 * full width and aligns its buttons right, so a second button lines up beside
 * the first. FormDone reads in that row after a command completes. TableScreen stacks a table's filter form, its rows and its next
 * page with one gap between them, and gives a data table in it the height of the viewport.
 */

import { type JSX, Show } from "solid-js";

export interface FormActionsProps {
  readonly children: JSX.Element;
}

export function FormActions(props: FormActionsProps): JSX.Element {
  return (
    // Inside a table screen the gap already spaces the row, so the margin
    // applies only after the fields of a form.
    <div
      data-slot="form-actions"
      class="mt-4 flex w-full flex-wrap items-center justify-end gap-2 [[data-slot=table-screen]>&]:mt-0"
    >
      {props.children}
    </div>
  );
}

export interface FormDoneProps {
  /** True after the last submission of the form completed. */
  readonly when: boolean;
}

/** The line a form shows beside its buttons after its command completes. */
export function FormDone(props: FormDoneProps): JSX.Element {
  return (
    <Show when={props.when}>
      <p data-slot="form-done" role="status" class="text-sm text-muted-foreground">
        Completed.
      </p>
    </Show>
  );
}

export interface TableScreenProps {
  readonly children: JSX.Element;
}

export function TableScreen(props: TableScreenProps): JSX.Element {
  return (
    // A data table fills the height of its container, so the screen gives it
    // the height of the viewport below the page header and the actions row,
    // and never less than 32rem. A read never changes the height of the page.
    <section
      data-slot="table-screen"
      class="flex min-w-0 flex-col gap-4 [&>[data-slot=data-table]]:h-[calc(100svh-9rem)] [&>[data-slot=data-table]]:min-h-[32rem]"
    >
      {props.children}
    </section>
  );
}
