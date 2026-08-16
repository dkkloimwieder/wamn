import type { JSX } from "@solidjs/web";
import { For, Match, Show, Switch, createMemo } from "solid-js";

import type { JsonValue } from "../reader/types";
import { DataTable, type Column } from "../ui/data-table";
import { JsonView } from "../ui/json-view";
import { SectionLabel } from "../ui/section-label";
import { EmptyState } from "../ui/states";
import "./draft-structure.css";

/**
 * §2.4's structure tab: the flow a draft's bytes describe, when they describe
 * one.
 *
 * `DraftParse.ok` says the bytes are valid JSON. It says nothing else, and this
 * file may never read it as more: §2.4 is explicit that a half-finished draft is
 * a normal object here, not an error condition, so a document that parses and
 * carries no nodes at all is a thing an author opens this tab to look at. Every
 * field below is therefore decoded defensively, every lane says what it did not
 * find, and nothing here throws or renders a blank in place of a fact.
 *
 * The field names read below are the flow document's own — `crates/execution/`
 * `flow-model`'s `Node` and `Edge`, kebab-cased and `deny_unknown_fields`, so
 * `from-port` and `to-port` and `ordinal` are the only spellings the platform
 * gives a meaning to. A draft is stored as unparsed bytes and may therefore
 * spell anything, but a port lifted out of some other key would be a port this
 * screen invented, and an absent key here means what the platform's own default
 * says it means rather than "nothing was recorded".
 */

// ── decoding ───────────────────────────────────────────────────────────────

/**
 * A decoded JSON object. Its values are `| undefined` because reading a key the
 * document does not carry is exactly how this file finds out that it does not
 * carry it — the compiler is not asked to pretend every key is there.
 */
type JsonObject = { readonly [key: string]: JsonValue | undefined };

/** One node the document declares, in the four columns §2.4 draws. */
export type FlowNode = {
  /** 1-based position in the document's own list, so two nodes may share an id. */
  readonly position: number;
  readonly id: string;
  readonly type: string | null;
  readonly connection: string | null;
  /** `undefined` is a node with no config field; `null` is a config field of null. */
  readonly config: JsonValue | undefined;
};

export type FlowEdge = {
  readonly position: number;
  readonly from: string;
  /**
   * The document's own `from-port`, or null when it names none. Null is not "no
   * port": `Edge::from_port` defaults to `main` and is skip-serialized when it
   * *is* main, so the key is absent on exactly the edges that leave by the main
   * port — and present on exactly the branch ports an author came here to read.
   */
  readonly fromPort: string | null;
  readonly to: string;
  /**
   * The document's own `to-port`, or null when it names none. An `Option` with
   * no default on the platform's side either: an unnamed target port is the
   * node's default input.
   */
  readonly toPort: string | null;
  /**
   * The document's own `ordinal` — this edge's fan-out order within its
   * `(from, from-port)` group — or null when it names none.
   */
  readonly ordinal: number | null;
};

/**
 * One collection of the document, and what became of it. `absence` is why there
 * is no list at all — a missing field and a field that is not a list are
 * different facts and get different sentences — and `unreadable` counts the
 * entries a list held that name nothing renderable, so they are subtracted in
 * words rather than dropped in silence.
 */
export type FlowLane<Item> = {
  readonly items: readonly Item[];
  readonly absence: string | null;
  readonly unreadable: number;
};

export type FlowShape = {
  readonly nodes: FlowLane<FlowNode>;
  readonly edges: FlowLane<FlowEdge>;
};

/**
 * The casts here are deliberate and local: `Array.isArray`'s predicate is
 * `any[]`, which does not narrow a `readonly` member of a union, so both guards
 * answer with the type they checked for rather than depending on how the
 * compiler narrows through the built-in.
 */
function objectOf(value: JsonValue): JsonObject | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as JsonObject)
    : null;
}

function listOf(value: JsonValue): readonly JsonValue[] | null {
  return Array.isArray(value) ? (value as readonly JsonValue[]) : null;
}

function stringOf(value: JsonValue | undefined): string | null {
  return typeof value === "string" ? value : null;
}

/** What a value is, in the words the lanes' sentences read it out with. */
function shapeName(value: JsonValue): string {
  if (value === null) {
    return "null";
  }
  if (Array.isArray(value)) {
    return "a list";
  }
  return typeof value === "object" ? "an object" : `a ${typeof value}`;
}

type DocumentRead =
  | { readonly kind: "object"; readonly object: JsonObject }
  /** Unreachable from the screen, which only mounts this tab on a parse that succeeded. */
  | { readonly kind: "unparsed" }
  | { readonly kind: "other"; readonly shape: string };

function readDocument(definition: string): DocumentRead {
  let parsed: JsonValue;
  try {
    parsed = JSON.parse(definition) as JsonValue;
  } catch {
    return { kind: "unparsed" };
  }
  const object = objectOf(parsed);
  return object === null ? { kind: "other", shape: shapeName(parsed) } : { kind: "object", object };
}

function lane<Item>(
  document: DocumentRead,
  field: string,
  read: (entry: JsonValue, position: number) => Item | null,
): FlowLane<Item> {
  if (document.kind === "unparsed") {
    return {
      items: [],
      absence: `these bytes are not valid JSON, so they declare no ${field}`,
      unreadable: 0,
    };
  }
  if (document.kind === "other") {
    return {
      items: [],
      absence: `this document is ${document.shape} rather than an object, so it declares no ${field}`,
      unreadable: 0,
    };
  }
  const raw = document.object[field];
  if (raw === undefined) {
    return { items: [], absence: `this document carries no "${field}" field`, unreadable: 0 };
  }
  const list = listOf(raw);
  if (list === null) {
    return {
      items: [],
      absence: `this document's "${field}" field is ${shapeName(raw)} rather than a list`,
      unreadable: 0,
    };
  }

  const items: Item[] = [];
  let unreadable = 0;
  for (const [index, entry] of list.entries()) {
    const item = read(entry, index + 1);
    if (item === null) {
      unreadable += 1;
    } else {
      items.push(item);
    }
  }
  return { items, absence: null, unreadable };
}

/** A node with no id names nothing: it cannot be a row, and it cannot be an edge's end. */
function readNode(entry: JsonValue, position: number): FlowNode | null {
  const object = objectOf(entry);
  if (object === null) {
    return null;
  }
  const id = stringOf(object["id"]);
  if (id === null) {
    return null;
  }
  return {
    position,
    id,
    type: stringOf(object["type"]),
    connection: stringOf(object["connection"]),
    config: object["config"],
  };
}

/**
 * The document's declared fan-out order. `Edge::ordinal` is a `u32`, so a float,
 * a negative or a string is a value the platform would refuse — read as naming
 * no order rather than as an order of its own.
 */
function ordinalOf(value: JsonValue | undefined): number | null {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : null;
}

/** An edge missing either end joins nothing, and cannot be grouped under a source. */
function readEdge(entry: JsonValue, position: number): FlowEdge | null {
  const object = objectOf(entry);
  if (object === null) {
    return null;
  }
  const from = stringOf(object["from"]);
  const to = stringOf(object["to"]);
  if (from === null || to === null) {
    return null;
  }
  return {
    position,
    from,
    fromPort: stringOf(object["from-port"]),
    to,
    toPort: stringOf(object["to-port"]),
    ordinal: ordinalOf(object["ordinal"]),
  };
}

export function readFlow(definition: string): FlowShape {
  const document = readDocument(definition);
  return {
    nodes: lane(document, "nodes", readNode),
    edges: lane(document, "edges", readEdge),
  };
}

export type EdgeGroup = { readonly from: string; readonly edges: readonly FlowEdge[] };

/** The port the platform fills in for an edge that names no `from-port`. */
const MAIN_PORT = "main";

/** The port an edge leaves by, as the platform reads it and not as the document spells it. */
export function leavingPort(edge: FlowEdge): string {
  return edge.fromPort ?? MAIN_PORT;
}

/** One edge with the two things the document does not say outright: its port, and its dispatch order. */
type Dispatched = { readonly edge: FlowEdge; readonly port: string; readonly order: number };

/**
 * The group an edge is ordered within — `(from, from-port)`, the only pair
 * `flow-model` compares ordinals inside. The two are joined by NUL rather than
 * by a printable character an id could carry itself.
 */
function groupKey(edge: FlowEdge): string {
  return `${edge.from}\u0000${leavingPort(edge)}`;
}

/**
 * Every edge's fan-out order — the sequence the engine dispatches a branch's
 * targets in.
 *
 * `flow-model` is explicit that this is the edge's own `ordinal` and never its
 * position in the `edges` array. An authored document may omit it, and the
 * platform then materializes the edge's position *within its `(from, from-port)`
 * group* at parse, with the counter advancing for every edge of the group so an
 * author's explicit ordinal is never overwritten and never shifts the ones
 * around it. That rule is reproduced exactly here, so the order this tab shows
 * is the engine's and not the console's reading of array position.
 */
function dispatched(edges: readonly FlowEdge[]): readonly Dispatched[] {
  const positions = new Map<string, number>();
  return edges.map((edge) => {
    const key = groupKey(edge);
    const position = positions.get(key) ?? 0;
    positions.set(key, position + 1);
    return { edge, port: leavingPort(edge), order: edge.ordinal ?? position };
  });
}

/**
 * Within one source node: the ports in the order the document first names each,
 * and within a port the engine's fan-out order. Two edges leaving different
 * ports never compare (`flow-model`), so the ports themselves keep document
 * order rather than being sorted into a sequence nothing declares.
 */
function inDispatchOrder(grouped: readonly Dispatched[]): readonly FlowEdge[] {
  const ports = [...new Set(grouped.map((entry) => entry.port))];
  return [...grouped]
    .sort((left, right) => {
      const byPort = ports.indexOf(left.port) - ports.indexOf(right.port);
      // `sort` is stable, so two edges a document gave the same ordinal keep the
      // order it wrote them in; the platform has no better tie-break either.
      return byPort === 0 ? left.order - right.order : byPort;
    })
    .map((entry) => entry.edge);
}

/**
 * §2.4's `grouped by source node`, in the order the document first names each
 * source. A cycle needs no case of its own: an edge back to an earlier node is
 * an ordinary edge of its own source's group, and reading it as anything else
 * would be this screen inventing a verdict about a draft nobody has run.
 */
export function groupEdges(edges: readonly FlowEdge[]): readonly EdgeGroup[] {
  const groups = new Map<string, Dispatched[]>();
  for (const entry of dispatched(edges)) {
    const group = groups.get(entry.edge.from);
    if (group === undefined) {
      groups.set(entry.edge.from, [entry]);
    } else {
      group.push(entry);
    }
  }
  return [...groups].map(([from, grouped]) => ({ from, edges: inDispatchOrder(grouped) }));
}

// ── rendering ──────────────────────────────────────────────────────────────

/**
 * A field the document does not carry, in the shape §2.2's duration column
 * already gives one: the dash keeps the row's columns aligned and the words
 * beside it are what a reader is actually told. The table's footer says once
 * what every dash on it means, so a column of them is not a column of blanks.
 */
function Absent(props: { what: string }): JSX.Element {
  return (
    <span class="draft-structure-absent">
      —<span class="visually-hidden">{props.what}</span>
    </span>
  );
}

/** What a row says about a config the reader has not opened — JsonView's own spelling. */
export function configSummary(config: JsonValue): string {
  const list = listOf(config);
  if (list !== null) {
    return `[ ${list.length} ${list.length === 1 ? "item" : "items"} ]`;
  }
  const object = objectOf(config);
  if (object !== null) {
    const keys = Object.keys(object).length;
    return `{ ${keys} ${keys === 1 ? "key" : "keys"} }`;
  }
  return JSON.stringify(config);
}

const nodeColumns: ReadonlyArray<Column<FlowNode>> = [
  { header: "id", cell: (node) => node.id },
  { header: "type", cell: (node) => node.type ?? <Absent what="no type recorded" /> },
  {
    header: "connection",
    // No fixture carries one, and no draft this console has seen does: the
    // column stands because §2.4 draws it, and states its own absence rather
    // than leaving six blank cells that read as six nodes needing no connection.
    cell: (node) => node.connection ?? <Absent what="no connection recorded" />,
  },
  {
    header: "config",
    cell: (node) =>
      node.config === undefined ? (
        <Absent what="no config recorded" />
      ) : (
        <span class="draft-structure-config">{configSummary(node.config)}</span>
      ),
  },
];

/** Why an entry the document held is not on the screen, stated rather than dropped. */
function unlisted(count: number, field: string, missing: string): string {
  return count === 1
    ? `one entry of "${field}" has ${missing}, so it is not listed`
    : `${count} entries of "${field}" have ${missing}, so they are not listed`;
}

/**
 * The ids this document names more than once. A half-finished draft may name
 * one twice, and the two rows stay two rows — but their two toggles would then
 * carry one name, which §3's floor forbids as squarely as a blank one.
 */
function repeatedIds(nodes: readonly FlowNode[]): ReadonlySet<string> {
  const seen = new Set<string>();
  const repeated = new Set<string>();
  for (const node of nodes) {
    if (seen.has(node.id)) {
      repeated.add(node.id);
    }
    seen.add(node.id);
  }
  return repeated;
}

/**
 * What a row's controls are named for. A repeated id is told apart by the
 * document's own position rather than by a count this screen would make up.
 */
export function nodeLabel(node: FlowNode, repeated: ReadonlySet<string>): string {
  return repeated.has(node.id) ? `node ${node.id} at position ${node.position}` : `node ${node.id}`;
}

/**
 * A row's identity across a re-read.
 *
 * The id where the document names it once, so a reader's open row follows its
 * node when a node above it comes or goes; the id *and* its position where the
 * document names it twice, because a repeated id identifies nothing and a
 * `keyed` list may not be handed one key for two rows. Position alone would be
 * the cheaper key and is the wrong one: a re-read that drops a node moves every
 * position above it, and the reader's open row lands on a node they never
 * opened. Positions are 1-based, so the unique form can never collide with the
 * repeated one.
 */
export function nodeKey(node: FlowNode, repeated: ReadonlySet<string>): string {
  return `${repeated.has(node.id) ? node.position : ""}\u0000${node.id}`;
}

function NodesLane(props: { lane: FlowLane<FlowNode> }): JSX.Element {
  const repeated = createMemo(() => repeatedIds(props.lane.items));
  const footer = () => {
    const notes: string[] = [];
    if (props.lane.unreadable > 0) {
      notes.push(unlisted(props.lane.unreadable, "nodes", "no id"));
    }
    if (props.lane.items.length > 0) {
      notes.push("— marks a field this document does not carry");
    }
    return notes.length === 0 ? null : notes.join(" · ");
  };

  return (
    <div class="draft-structure-lane">
      <SectionLabel label="nodes" />
      <DataTable
        caption="the nodes this draft's document declares"
        columns={nodeColumns}
        rows={props.lane.items}
        rowKey={(node) => nodeKey(node, repeated())}
        disclosure={(node) => {
          const config = node.config;
          // Nothing to open is not an empty disclosure: the config column has
          // already said the field is not there.
          if (config === undefined) {
            return null;
          }
          /*
           * The row and the view inside it are two controls acting on two
           * different things — one opens the row, one copies the document — so
           * they may not share a name (§3's floor). The row is named for the
           * row; the evidence in it is named for the evidence.
           */
          const label = nodeLabel(node, repeated());
          return {
            label,
            content: () => <JsonView value={config} subject={`${label} config`} />,
          };
        }}
        empty={
          <EmptyState
            region="nodes"
            reason={props.lane.absence ?? `this document's "nodes" list is empty`}
          />
        }
        footer={footer()}
      />
    </div>
  );
}

/**
 * What the platform reads at one end of an edge whose port the document does
 * not name. Never "no port recorded": an unnamed port is not an absence on
 * either end — it is the value the engine will use — so the end shows that
 * value, parenthesized as one the document does not contain.
 */
type UnnamedPort = { readonly shown: string; readonly said: string };

const UNNAMED_FROM: UnnamedPort = {
  shown: MAIN_PORT,
  said: `no from-port named — ${MAIN_PORT}, the platform's default`,
};

const UNNAMED_TO: UnnamedPort = {
  shown: "default input",
  said: "no to-port named — the target node's default input",
};

/** One end of an edge: the node, and the port it is joined by. */
function EdgeEnd(props: { node: string; port: string | null; unnamed: UnnamedPort }): JSX.Element {
  return (
    <span class="draft-edge-end">
      {props.node}
      <span aria-hidden="true">[</span>
      <Switch>
        {/*
         * Identity, not truthiness, and in this order: a document that names no
         * port and a document that names the empty string are two facts, and
         * only the first of them has a platform default standing behind it.
         */}
        <Match when={props.port === null}>
          <span class="draft-edge-port-default">
            ({props.unnamed.shown})<span class="visually-hidden">{props.unnamed.said}</span>
          </span>
        </Match>
        <Match when={props.port === ""}>
          <span>
            ""
            <span class="visually-hidden">this document names this port as the empty string</span>
          </span>
        </Match>
        <Match when={props.port !== null}>{props.port}</Match>
      </Switch>
      <span aria-hidden="true">]</span>
    </span>
  );
}

/**
 * Said once under the lane rather than on every row it applies to: what a
 * parenthesized port is. Without it the parentheses are a shape carrying a
 * meaning nothing on the screen states.
 */
export function portNote(edges: readonly FlowEdge[]): string | null {
  const unnamed = edges.some((edge) => edge.fromPort === null || edge.toPort === null);
  return unnamed
    ? `a port in parentheses is one this document does not name: the platform reads an unnamed from-port as ${MAIN_PORT}, and an unnamed to-port as the target node's default input`
    : null;
}

/**
 * Where the order inside each group came from. The reader is told, because the
 * order is a claim about dispatch and not about the screen: an author debugging
 * a fan-out has to know whether they are reading their own `ordinal`s or the
 * positions the platform will materialize from document order.
 */
export function orderNote(edges: readonly FlowEdge[]): string | null {
  // Only a port that dispatches more than one edge has an order at all, so a
  // document without a fan-out is told nothing about one.
  const groups = new Set<string>();
  const fansOut = edges.some((edge) => {
    const key = groupKey(edge);
    const seen = groups.has(key);
    groups.add(key);
    return seen;
  });
  if (!fansOut) {
    return null;
  }
  const named = edges.filter((edge) => edge.ordinal !== null).length;
  if (named === 0) {
    return "no edge names an ordinal, so within each port these stand in document order — the order the platform materializes at parse";
  }
  if (named === edges.length) {
    return "within each port, edges stand in the order their own ordinal names";
  }
  return `within each port, ${named} of ${edges.length} edges stand in the order their own ordinal names and the rest take their position in the group, as the platform does at parse`;
}

function EdgesLane(props: { lane: FlowLane<FlowEdge> }): JSX.Element {
  const groups = createMemo(() => groupEdges(props.lane.items));

  return (
    <div class="draft-structure-lane">
      <SectionLabel label="edges" />
      <Show
        when={groups().length > 0}
        fallback={
          <EmptyState
            region="edges"
            reason={props.lane.absence ?? `this document's "edges" list is empty`}
          />
        }
      >
        <ul class="draft-edge-groups" role="list">
          <For each={groups()}>
            {(group) => (
              <li class="draft-edge-group">
                {/* The source is data, so it wears the data face and never the
                    uppercase label role (§1.2). */}
                <p class="draft-edge-source">from {group.from}</p>
                <ul class="draft-edges" role="list">
                  <For each={group.edges}>
                    {(edge) => (
                      <li class="draft-edge">
                        <EdgeEnd node={edge.from} port={edge.fromPort} unnamed={UNNAMED_FROM} />
                        <span class="draft-edge-arrow">
                          <span aria-hidden="true">─→</span>
                          <span class="visually-hidden">to</span>
                        </span>
                        <EdgeEnd node={edge.to} port={edge.toPort} unnamed={UNNAMED_TO} />
                      </li>
                    )}
                  </For>
                </ul>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={props.lane.unreadable > 0}>
        <p class="draft-structure-note frame">
          {unlisted(props.lane.unreadable, "edges", "no from or to")}
        </p>
      </Show>
      <Show when={orderNote(props.lane.items)}>
        {(note) => <p class="draft-structure-note frame">{note()}</p>}
      </Show>
      <Show when={portNote(props.lane.items)}>
        {(note) => <p class="draft-structure-note frame">{note()}</p>}
      </Show>
    </div>
  );
}

/**
 * The tab's body: what the bytes declare, in §2.4's order — the nodes as a
 * table, then the edges as aligned rows grouped by source node.
 */
export function DraftStructure(props: { definition: string }): JSX.Element {
  const shape = createMemo(() => readFlow(props.definition));

  return (
    <div class="draft-structure">
      <NodesLane lane={shape().nodes} />
      <EdgesLane lane={shape().edges} />
    </div>
  );
}
