/**
 * One labeled control, with the refusal that marks it.
 *
 * A generated component names these and states the label, the value and the
 * handler. The composition of Field, Label, Input, Select and FieldError, and
 * the id that links a label to its control, belong here.
 */

import { createUniqueId, type JSX, Show } from "solid-js";

import { Checkbox } from "./components/ui/checkbox";
import { Field, FieldError, FieldLabel } from "./components/ui/field";
import { Input } from "./components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./components/ui/select";

export interface TextFieldProps {
  readonly label: string;
  readonly type: "text" | "number";
  readonly value?: string | undefined;
  readonly min?: number | undefined;
  readonly max?: number | undefined;
  /** Called on every keystroke. */
  readonly onInput?: ((value: string) => void) | undefined;
  /** Called when the operator commits the value. */
  readonly onChange?: ((value: string) => void) | undefined;
  /** The refusal that marks this control, or null. */
  readonly error?: string | null | undefined;
}

export function TextField(props: TextFieldProps): JSX.Element {
  const id = createUniqueId();
  return (
    <Field data-invalid={props.error ? "true" : undefined}>
      <FieldLabel for={id}>{props.label}</FieldLabel>
      <Input
        id={id}
        type={props.type}
        value={props.value ?? ""}
        min={props.min ?? ""}
        max={props.max ?? ""}
        aria-invalid={props.error ? "true" : undefined}
        onInput={(event) => props.onInput?.(event.currentTarget.value)}
        onChange={(event) => props.onChange?.(event.currentTarget.value)}
      />
      <Show when={props.error}>
        <FieldError>{props.error}</FieldError>
      </Show>
    </Field>
  );
}

export interface CheckFieldProps {
  readonly label: string;
  readonly checked: boolean;
  readonly onChange: (checked: boolean) => void;
  /** The refusal that marks this control, or null. */
  readonly error?: string | null | undefined;
}

export function CheckField(props: CheckFieldProps): JSX.Element {
  const id = createUniqueId();
  return (
    <Field orientation="horizontal" data-invalid={props.error ? "true" : undefined}>
      <Checkbox id={id} checked={props.checked} onChange={(checked) => props.onChange(checked)} />
      <FieldLabel for={id}>{props.label}</FieldLabel>
      <Show when={props.error}>
        <FieldError>{props.error}</FieldError>
      </Show>
    </Field>
  );
}

/** One value the contract permits, and the text an operator reads for it. */
export interface Choice {
  readonly value: string;
  readonly text: string;
}

export interface ChoiceFieldProps {
  readonly label: string;
  /** Exactly the values the contract permits, in contract order. */
  readonly choices: readonly Choice[];
  /** True when the input may be left empty. The empty choice sends "". */
  readonly allowEmpty: boolean;
  readonly value?: string | null | undefined;
  readonly onChange: (value: string) => void;
  /** The refusal that marks this control, or null. */
  readonly error?: string | null | undefined;
}

const EMPTY: Choice = { value: "", text: "" };

export function ChoiceField(props: ChoiceFieldProps): JSX.Element {
  const labelId = createUniqueId();
  const options = (): Choice[] => (props.allowEmpty ? [EMPTY, ...props.choices] : [...props.choices]);
  const selected = (): Choice | null =>
    options().find((choice) => choice.value === (props.value ?? "")) ?? null;
  return (
    <Field data-invalid={props.error ? "true" : undefined}>
      <FieldLabel id={labelId}>{props.label}</FieldLabel>
      <Select<Choice>
        options={options()}
        optionValue="value"
        optionTextValue="text"
        value={selected()}
        onChange={(choice) => props.onChange(choice?.value ?? "")}
        itemComponent={(item) => <SelectItem item={item.item}>{item.item.rawValue.text}</SelectItem>}
      >
        <SelectTrigger aria-labelledby={labelId} aria-invalid={props.error ? "true" : undefined}>
          <SelectValue<Choice>>{(state) => state.selectedOption()?.text}</SelectValue>
        </SelectTrigger>
        <SelectContent />
      </Select>
      <Show when={props.error}>
        <FieldError>{props.error}</FieldError>
      </Show>
    </Field>
  );
}
