/**
 * One control that chooses a record from the rows a list returned.
 *
 * The generated selector owns the read: it holds the rows, sends the search
 * and follows the cursor. This control shows those rows, reports the text the
 * operator typed, and offers the next page. It filters nothing itself, so the
 * options are exactly the rows the release sent.
 */

import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  on,
  onCleanup,
  Show,
} from "solid-js";

import { Button } from "./components/ui/button";
import { Field, FieldError, FieldLabel } from "./components/ui/field";
import {
  Combobox,
  ComboboxContent,
  ComboboxInput,
  ComboboxItem,
} from "./components/ui/combobox";

/** How long the operator pauses typing before the search is sent. */
export const SEARCH_PAUSE_MS = 300;

export interface RecordSelectProps<Row extends object> {
  /** The rows the list returned, in the order it returned them. */
  readonly options: readonly Row[];
  /** The value one row stands for, which the form stores. */
  readonly optionValue: (row: Row) => string;
  /** The text one row shows. */
  readonly optionLabel: (row: Row) => string;
  /** The stored value, or null when no record is chosen. */
  readonly value: string | null;
  readonly onChange: (value: string | null) => void;
  /** The label of the control. */
  readonly label: string;
  /** The refusal that marks this control, or null. */
  readonly error?: string | null | undefined;
  /**
   * Called with the typed text after a pause. A selector whose list declares
   * no search passes none, and its input is then read only.
   */
  readonly onSearch?: (text: string) => void;
  /** True while the last reply carried a cursor. */
  readonly hasNextPage?: boolean;
  readonly onNextPage?: () => void;
}

export function RecordSelect<Row extends object>(props: RecordSelectProps<Row>) {
  // The chosen row stays shown after a search replaces the options.
  const [chosen, setChosen] = createSignal<Row | null>(null);
  const key = (row: Row | null): string | null => (row === null ? null : props.optionValue(row));
  // The combobox writes the chosen label back into the input whenever the
  // selection changes identity. A search reply replaces every row, so the
  // chosen row stays the same object while its value stays the same.
  const selected = createMemo((): Row | null => {
    const value = props.value;
    if (value === null || value === "") {
      return null;
    }
    const listed = props.options.find((row) => props.optionValue(row) === value);
    if (listed !== undefined) {
      return listed;
    }
    const kept = chosen();
    return kept !== null && props.optionValue(kept) === value ? kept : null;
  }, null, { equals: (previous, next) => key(previous) === key(next) });
  createEffect(
    on(selected, (row) => {
      if (row !== null) {
        setChosen(() => row);
      }
    }),
  );

  let pending: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(pending));
  const typed = (text: string) => {
    const search = props.onSearch;
    const row = selected();
    // Choosing a row writes its label into the input, which is not a search.
    if (search === undefined || (row !== null && props.optionLabel(row) === text)) {
      return;
    }
    clearTimeout(pending);
    pending = setTimeout(() => search(text), SEARCH_PAUSE_MS);
  };

  const labelId = createUniqueId();
  return (
    <Field data-invalid={props.error ? "true" : undefined}>
      <FieldLabel id={labelId}>{props.label}</FieldLabel>
      <Combobox<Row>
        options={[...props.options]}
        optionValue={(row) => props.optionValue(row)}
        optionTextValue={(row) => props.optionLabel(row)}
        optionLabel={(row) => props.optionLabel(row)}
        value={selected()}
        onChange={(row) => {
          setChosen(() => row);
          props.onChange(row === null ? null : props.optionValue(row));
        }}
        onInputChange={typed}
        defaultFilter={() => true}
        triggerMode={props.onSearch === undefined ? "focus" : "input"}
        itemComponent={(item) => (
          <ComboboxItem item={item.item}>{item.item.textValue}</ComboboxItem>
        )}
      >
        <ComboboxInput
          aria-labelledby={labelId}
          aria-invalid={props.error ? "true" : undefined}
          readOnly={props.onSearch === undefined}
        />
        <ComboboxContent
          footer={
            <Show when={props.hasNextPage === true && props.onNextPage !== undefined}>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                class="w-full"
                aria-label={`${props.label} next page`}
                // The input keeps the focus, so the list stays open.
                onMouseDown={(event: MouseEvent) => event.preventDefault()}
                onClick={() => props.onNextPage?.()}
              >
                next page
              </Button>
            </Show>
          }
        />
      </Combobox>
      <Show when={props.error}>
        <FieldError>{props.error}</FieldError>
      </Show>
    </Field>
  );
}
