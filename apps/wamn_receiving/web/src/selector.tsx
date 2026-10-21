import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Button, Combobox } from "@wfe/components";
import type { CustomFieldProps } from "@wfe/generator";
import type { OperationRoute, Transport } from "@wamn/web-runtime";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";

type Choice = { value: string; label: string };

/** A described reference field with application-owned requests and accepted values. */
export function Selector(
  props: Omit<CustomFieldProps<{ value: string | undefined }, "value">, "onValueChange"> & {
    onValueChange: (value: string) => void;
    transport: Transport;
    route: OperationRoute;
    keyField: string;
    displayField: string;
    label: string;
    searchField?: string;
    input?: RecordValue;
    unavailable?: boolean;
  },
) {
  const [choices, setChoices] = createSignal<Choice[]>([]);
  const [search, setSearch] = createSignal("");
  const [cursor, setCursor] = createSignal<string>();
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string>();
  let generation = 0;
  let active = true;
  onCleanup(() => {
    active = false;
    generation += 1;
  });
  const load = async (input: RecordValue, search: string, next = false) => {
    const current = ++generation;
    setLoading(true);
    setError(undefined);
    if (!next) {
      setChoices([]);
      setCursor(undefined);
    }
    try {
      const result = await invoke<Rows>(props.transport, props.route, {
        ...input,
        ...(props.searchField && search ? { filter: { [props.searchField]: [search] } } : {}),
        ...(next && cursor() ? { cursor: cursor()! } : {}),
      });
      if (!active || current !== generation) return;
      const options = rowsOf(result)
        .filter((row) => row[props.keyField] !== null)
        .map((row) => ({ value: String(row[props.keyField]), label: String(row[props.displayField]) }));
      setChoices(next ? [...choices(), ...options] : options);
      setCursor(result.next_cursor ?? undefined);
    } catch (error) {
      if (active && current === generation) setError(String(error));
    } finally {
      if (active && current === generation) setLoading(false);
    }
  };
  createEffect(
    () => ({ input: props.input ?? {}, unavailable: props.unavailable, search: props.searchField ? search() : "" }),
    ({ input, unavailable, search }) => {
      if (unavailable) {
        generation += 1;
        setChoices([]);
        setCursor(undefined);
        setLoading(false);
      } else void load(input, search);
    },
  );
  const selected = () =>
    choices().find((choice) => choice.value === props.value) ??
    (props.value ? { value: props.value, label: props.value } : null);
  return (
    <Combobox<Choice>
      options={choices()}
      optionValue="value"
      optionTextValue="label"
      optionLabel="label"
      triggerMode="focus"
      disallowEmptySelection
      value={selected()}
      onChange={(choice) => props.onValueChange(choice?.value ?? "")}
      disabled={props.disabled || props.readOnly || props.unavailable}
      onInputChange={(value) => setSearch(value)}
      itemComponent={(item) => (
        <Combobox.Item item={item.item}>
          <Combobox.ItemLabel>{item.item.rawValue.label}</Combobox.ItemLabel>
        </Combobox.Item>
      )}
    >
      <Combobox.Control>
        <Combobox.Input id={props.id} aria-label={props.label} />
        <Combobox.Trigger aria-label={props.label} />
      </Combobox.Control>
      <Combobox.Portal>
        <Combobox.Content
          status={error() ?? (loading() ? "Loading" : undefined)}
          footer={
            <Show when={cursor()}>
              <Button disabled={loading()} onClick={() => void load(props.input ?? {}, search(), true)}>
                Next page
              </Button>
            </Show>
          }
        >
          <Combobox.Listbox />
        </Combobox.Content>
      </Combobox.Portal>
    </Combobox>
  );
}
