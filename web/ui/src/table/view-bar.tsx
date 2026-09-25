/**
 * The view controls of the DataTable toolbar (wamn-9v2r.2).
 *
 * The picker applies a saved view. A name and save keep the current state as
 * a view, rename gives the chosen view the typed name, delete removes it, and
 * reset applies the table definition's state. The views live in memory for
 * the session. The bar also reports, once, what the URL named that the table
 * ignored.
 */

import { createSignal, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { ChoiceField, TextField } from "../fields";

export function ViewBar(props: {
  /** The names of the saved views, in the order they were saved. */
  names: readonly string[];
  /** The name of the view in force, or null. */
  chosen: string | null;
  /** What the URL named that the table ignored. */
  ignored: readonly string[];
  onPick: (name: string) => void;
  onSave: (name: string) => void;
  onRename: (from: string, to: string) => void;
  onDelete: (name: string) => void;
  onReset: () => void;
}): JSX.Element {
  const [name, setName] = createSignal("");
  const typed = () => name().trim();
  return (
    <div data-slot="data-table-views" class="flex flex-wrap items-end gap-2">
      <div class="w-40">
        <ChoiceField
          label="view"
          choices={props.names.map((view) => ({ value: view, text: view }))}
          value={props.chosen ?? ""}
          allowEmpty={false}
          onChange={(view) => {
            if (view !== "") {
              props.onPick(view);
            }
          }}
        />
      </div>
      <div class="w-36">
        <TextField label="view name" type="text" value={name()} onInput={(value) => setName(value)} />
      </div>
      <Button
        type="button"
        variant="outline"
        disabled={typed() === ""}
        onClick={() => {
          props.onSave(typed());
          setName("");
        }}
      >
        save view
      </Button>
      <Button
        type="button"
        variant="ghost"
        disabled={props.chosen === null || typed() === "" || props.names.includes(typed())}
        onClick={() => {
          props.onRename(props.chosen!, typed());
          setName("");
        }}
      >
        rename
      </Button>
      <Button
        type="button"
        variant="ghost"
        disabled={props.chosen === null}
        onClick={() => props.onDelete(props.chosen!)}
      >
        delete
      </Button>
      <Button type="button" variant="ghost" onClick={() => props.onReset()}>
        reset view
      </Button>
      <Show when={props.ignored.length > 0}>
        <p data-slot="data-table-url-ignored" class="w-full text-sm text-muted-foreground">
          The address named what this table does not have, so it was ignored: {props.ignored.join(", ")}
        </p>
      </Show>
    </div>
  );
}
