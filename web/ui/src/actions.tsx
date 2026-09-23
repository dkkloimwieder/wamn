/**
 * The layout a generated screen names, so that it states no class itself.
 *
 * FormActions closes a form or a table with its buttons. The row spans the
 * full width and aligns its buttons right, so a second button lines up beside
 * the first. TableScreen stacks a table's filter form, its rows and its next
 * page with one gap between them.
 */

import type { JSX } from "solid-js";

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

export interface TableScreenProps {
  readonly children: JSX.Element;
}

export function TableScreen(props: TableScreenProps): JSX.Element {
  return (
    <section data-slot="table-screen" class="flex min-w-0 flex-col gap-4">
      {props.children}
    </section>
  );
}
