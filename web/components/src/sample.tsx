/**
 * The hand-written sample that proves this harness can check a component.
 *
 * It imports the runtime by name and one generated binding by path, exactly as
 * an emitted component does. When the emitter lands in `wamn-rs5b.3`, the
 * generated components become the real subject of the check, and this stays as
 * the smallest thing that fails when the harness itself breaks.
 */

import { createResource, For, Show } from "solid-js";

import type { JsonValue, Transport } from "@wamn/web-runtime";

import { list } from "../fixture/widget.js";

export interface SampleProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The selector that the fixture operation takes. */
  readonly selector: JsonValue;
}

/** One bounded list, rendered as plain rows. */
export function Sample(props: SampleProps) {
  const [outcome] = createResource(
    () => props.selector,
    // `makerId` is the input that narrows the list to one maker. It is
    // nullable and present, which is how this platform spells "not chosen".
    (selector: JsonValue) =>
      list(props.transport, [{ selector, makerId: null }]),
  );
  const rows = () => {
    const read = outcome();
    return read?.status === "completed" ? read.value.rows : [];
  };
  return (
    <div>
      <Show when={outcome()?.status !== "completed"}>
        <p>{outcome()?.status ?? "reading"}</p>
      </Show>
      <ul>
        <For each={rows()}>{(row) => <li>{row.id}</li>}</For>
      </ul>
    </div>
  );
}
