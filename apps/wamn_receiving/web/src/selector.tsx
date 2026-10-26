import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Button, Combobox } from "@wfe/components";
import type { CustomFieldProps } from "@wfe/generator";
import type { Transport } from "@wamn/web-runtime";
import type { ReferencePresentation } from "./generated/wamn_receiving/components/index.js";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";
import { referenceLabels, referenceRoute } from "./references";

type Choice = { value: string; label: string };

/** A described reference field with application-owned requests and accepted values. */
export function Selector(
  props: Omit<CustomFieldProps<{ value: string | undefined }, "value">, "onValueChange"> & {
    onValueChange: (value: string) => void;
    transport: Transport;
    reference: ReferencePresentation;
    label: string;
    input?: RecordValue;
    unavailable?: boolean;
  },
) {
  const [choices, setChoices] = createSignal<Choice[]>([]);
  const [search, setSearch] = createSignal("");
  const [cursor, setCursor] = createSignal<string>();
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [held, setHeld] = createSignal<Choice | null>(null);
  const label = referenceLabels(props.transport);
  let heldGeneration = 0;
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
      const reference = props.reference;
      const result = await invoke<Rows>(props.transport, referenceRoute(reference.operation), {
        ...input,
        ...(reference.searchField && search ? { filter: { [reference.searchField]: [search] } } : {}),
        ...(next && cursor() && reference.cursorInput ? { [reference.cursorInput]: cursor()! } : {}),
      });
      if (!active || current !== generation) return;
      const options = rowsOf(result)
        .filter((row) => row[reference.keyField] != null)
        .map((row) => ({
          value: String(row[reference.keyField]),
          label: String(row[reference.displayField] ?? "Unavailable"),
        }));
      setChoices(next ? [...choices(), ...options] : options);
      setCursor(result.next_cursor ?? undefined);
    } catch (error) {
      if (active && current === generation) setError(String(error));
    } finally {
      if (active && current === generation) setLoading(false);
    }
  };
  createEffect(
    () => ({
      input: props.input ?? {},
      unavailable: props.unavailable,
      search: props.reference.searchField ? search() : "",
    }),
    ({ input, unavailable, search }) => {
      if (unavailable) {
        generation += 1;
        setChoices([]);
        setCursor(undefined);
        setLoading(false);
      } else void load(input, search);
    },
  );
  createEffect(
    () => ({ value: props.value, input: props.input ?? {}, unavailable: props.unavailable }),
    ({ value, input, unavailable }) => {
      const current = ++heldGeneration;
      setHeld(null);
      if (!value || unavailable) return;
      void label(props.reference, value, input)
        .catch(() => "Unavailable")
        .then((text) => {
          if (active && current === heldGeneration) setHeld({ value, label: text });
        });
    },
  );
  const selected = () =>
    choices().find((choice) => choice.value === props.value) ?? (held()?.value === props.value ? held() : null);
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
