import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { loopingDraft, parsingDraft, unattributedDraft } from "../reader/fixtures";
import { DraftStructure, groupEdges, readFlow } from "./draft-structure";

afterEach(cleanup);

/**
 * A document spelled the way the platform spells one.
 *
 * `crates/execution/flow-model`'s `Edge` names an edge's ports `from-port` and
 * `to-port` and carries fan-out order as an explicit `ordinal`, all under
 * `deny_unknown_fields` — so these are the only spellings a stored flow
 * document can use, and the fixtures spell them that way too.
 *
 * What is written here rather than read off a fixture is the one combination no
 * fixture draft holds: a source that branches on two *named* ports, one of them
 * twice, with a named `to-port` on one edge and an ordinal on two of the four.
 * The ordinary fan-out — two edges leaving one port, ordered against the order
 * the array lists them in — is the notifications fixture's, and is read off it.
 */
const platformDocument = `{
  "nodes": [
    { "id": "ingress", "type": "request" },
    { "id": "check-stock", "type": "conditional", "config": { "expr": "body.sku" } },
    { "id": "reserve", "type": "integrate", "connection": "warehouse" },
    { "id": "fetch-inventory", "type": "integrate" }
  ],
  "edges": [
    { "from": "ingress", "to": "check-stock" },
    { "from": "check-stock", "from-port": "backorder", "to": "fetch-inventory", "ordinal": 1 },
    { "from": "check-stock", "from-port": "in-stock", "to": "reserve", "to-port": "main" },
    { "from": "check-stock", "from-port": "backorder", "to": "reserve", "ordinal": 0 }
  ]
}`;

/** The rows of one lane's table, cell by cell, as a reader would read them across. */
function rows(container: HTMLElement): string[][] {
  return [...container.querySelectorAll("tbody .data-table-row")].map((row) =>
    [...row.querySelectorAll(".data-table-cell")].map((cell) =>
      (cell.textContent ?? "").replace(/\s+/g, " ").trim(),
    ),
  );
}

/** What a sighted reader sees: the text without the words only a screen reader gets. */
function seen(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll(".visually-hidden")) {
    hidden.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** What a screen reader is told in their place, in the order the rows stand. */
function spoken(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".draft-edge .visually-hidden")].map((said) =>
    (said.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

/** The `from x` heading of each group, in the order the groups stand. */
function groupHeadings(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".draft-edge-source")].map(
    (heading) => heading.textContent ?? "",
  );
}

/** Every edge row on the screen, as `from[port] ─→ to[port]` — the two ends and nothing else. */
function edgeRows(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".draft-edge")].map((row) =>
    [...row.querySelectorAll(".draft-edge-end")].map(seen).join(" ─→ "),
  );
}

/** Everything the lane says about itself under the rows. */
function laneNotes(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".draft-structure-note")].map((note) =>
    (note.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

describe("readFlow — a document that has a flow's shape", () => {
  it("reads §2.4's draft: every node the document declares", () => {
    const shape = readFlow(parsingDraft.definition);

    expect(shape.nodes.absence).toBeNull();
    expect(shape.nodes.unreadable).toBe(0);
    expect(shape.nodes.items.map((node) => node.id)).toEqual([
      "ingress",
      "parse-order",
      "check-stock",
      "fetch-inventory",
      "reserve",
      "respond",
    ]);
    expect(shape.nodes.items[1]).toMatchObject({
      position: 2,
      id: "parse-order",
      type: "transform",
    });
    expect(shape.edges.absence).toBeNull();
    expect(shape.edges.items).toHaveLength(6);
  });

  it("keeps a field the document does not carry apart from one it does", () => {
    const shape = readFlow(parsingDraft.definition);
    const [ingress, parseOrder] = shape.nodes.items;

    // no node of any fixture carries a connection, and none of them is claimed to
    expect(shape.nodes.items.every((node) => node.connection === null)).toBe(true);
    // `undefined` is a node with no config field at all; the two are not one fact
    expect(ingress.config).toBeUndefined();
    expect(parseOrder.config).toEqual({ expr: "body.order", ctx: { order: "body.order" } });
  });

  it("reads each port from the name the flow document gives it", () => {
    const edges = readFlow(platformDocument).edges.items;

    expect(edges[1]).toEqual({
      position: 2,
      from: "check-stock",
      fromPort: "backorder",
      to: "fetch-inventory",
      toPort: null,
      ordinal: 1,
    });
    expect(edges[2]).toEqual({
      position: 3,
      from: "check-stock",
      fromPort: "in-stock",
      to: "reserve",
      toPort: "main",
      ordinal: null,
    });
    // an edge naming neither port names neither: null here is the key's absence
    // and not a port called null
    expect(edges[0]).toMatchObject({ fromPort: null, toPort: null, ordinal: null });
  });

  it("reads no port out of a key the flow document does not define", () => {
    // `Edge` is `deny_unknown_fields`, so `out`/`in` are keys no stored flow
    // document can carry: a port lifted out of them would be one this console
    // invented, and the branch it named would be a branch nothing declared.
    const shape = readFlow(
      `{ "edges": [{ "from": "check-stock", "out": "in-stock", "to": "reserve", "in": "main" }] }`,
    );

    expect(shape.edges.items[0]).toMatchObject({
      from: "check-stock",
      fromPort: null,
      to: "reserve",
      toPort: null,
    });
  });

  it("reads an ordinal only where the platform would accept one", () => {
    // `Edge::ordinal` is a `u32`: a float, a negative or a string is a value the
    // platform refuses, so it names no order rather than an order of its own
    const ordinals = readFlow(
      `{ "edges": [
           { "from": "a", "to": "b", "ordinal": 0 },
           { "from": "c", "to": "d", "ordinal": 1.5 },
           { "from": "e", "to": "f", "ordinal": -1 },
           { "from": "g", "to": "h", "ordinal": "2" }
         ] }`,
    ).edges.items.map((edge) => edge.ordinal);

    expect(ordinals).toEqual([0, null, null, null]);
  });

  it("groups edges by source node, in the order the document first names each", () => {
    const groups = groupEdges(readFlow(parsingDraft.definition).edges.items);

    expect(groups.map((group) => group.from)).toEqual([
      "ingress",
      "parse-order",
      "check-stock",
      "fetch-inventory",
      "reserve",
    ]);
    // the one node that branches keeps both of its edges
    expect(groups[2].edges.map((edge) => edge.to)).toEqual(["reserve", "fetch-inventory"]);
  });

  it("orders a fan-out by the ordinal each edge names, never by array position", () => {
    const groups = groupEdges(readFlow(platformDocument).edges.items);
    const branch = groups[1];

    expect(branch.from).toBe("check-stock");
    // the document lists `→ fetch-inventory` (ordinal 1) before `→ reserve`
    // (ordinal 0), and the engine dispatches them the other way round: array
    // position is never the fan-out order
    expect(branch.edges.map((edge) => `${edge.fromPort} ${edge.to}`)).toEqual([
      "backorder reserve",
      "backorder fetch-inventory",
      "in-stock reserve",
    ]);
  });

  it("compares two edges only when they leave by the same port", () => {
    // ordinal 0 on one port and ordinal 0 on another declare nothing about each
    // other, so the ports keep the order the document first named them in
    const groups = groupEdges(
      readFlow(
        `{ "edges": [
             { "from": "a", "from-port": "second", "to": "y", "ordinal": 0 },
             { "from": "a", "from-port": "first", "to": "z", "ordinal": 0 }
           ] }`,
      ).edges.items,
    );

    expect(groups[0].edges.map((edge) => edge.fromPort)).toEqual(["second", "first"]);
  });

  it("gives an edge that names no ordinal its position within its own port's group", () => {
    // the platform materializes the missing value at parse from the edge's
    // position in its group — 1 here — so an edge that names 3 dispatches after
    // an edge that names nothing, whichever way round the array holds them
    const groups = groupEdges(
      readFlow(
        `{ "edges": [
             { "from": "a", "to": "late", "ordinal": 3 },
             { "from": "a", "to": "early" }
           ] }`,
      ).edges.items,
    );

    expect(groups[0].edges.map((edge) => edge.to)).toEqual(["early", "late"]);
  });

  it("orders the notifications draft's own fan-out by the ordinals that draft names", () => {
    const groups = groupEdges(readFlow(loopingDraft.definition).edges.items);
    const fanOut = groups[1];

    // A stored document, not a literal written for this file: the notifications
    // draft lists `→ next-subscriber` (ordinal 1) ahead of `→ record-delivery`
    // (ordinal 0), both leaving `notify-subscriber` by the port neither names,
    // so the engine dispatches them the other way round from the way the array
    // holds them.
    expect(fanOut.from).toBe("notify-subscriber");
    expect(fanOut.edges.map((edge) => [edge.to, edge.position, edge.ordinal])).toEqual([
      ["record-delivery", 3, 0],
      ["next-subscriber", 2, 1],
    ]);
  });

  it("reads a cycle as an ordinary edge of its own source's group", () => {
    const groups = groupEdges(readFlow(loopingDraft.definition).edges.items);

    // the edge back to an earlier node is `next-subscriber`'s, like any other
    expect(groups.map((group) => group.from)).toEqual([
      "ingress",
      "notify-subscriber",
      "next-subscriber",
    ]);
    expect(groups[2].edges).toEqual([
      {
        // the fourth edge the document lists, which is where the cycle stands
        // now that `notify-subscriber` fans out above it
        position: 4,
        from: "next-subscriber",
        // the loop leaves a named branch, which is what makes it a loop rather
        // than the node's ordinary continuation
        fromPort: "more",
        to: "notify-subscriber",
        toPort: null,
        ordinal: null,
      },
    ]);
  });
});

describe("readFlow — a document that parses and is not a flow", () => {
  it("says which field is missing rather than answering with nothing", () => {
    const shape = readFlow(`{ "name": "half-finished" }`);

    expect(shape.nodes.items).toEqual([]);
    expect(shape.nodes.absence).toBe(`this document carries no "nodes" field`);
    expect(shape.edges.absence).toBe(`this document carries no "edges" field`);
  });

  it("says a field that is not a list is not a list, and what it is instead", () => {
    const shape = readFlow(`{ "nodes": { "ingress": {} }, "edges": 4 }`);

    expect(shape.nodes.absence).toBe(`this document's "nodes" field is an object rather than a list`);
    expect(shape.edges.absence).toBe(`this document's "edges" field is a number rather than a list`);
  });

  it("says a document that is not an object is not one", () => {
    expect(readFlow("[1, 2, 3]").nodes.absence).toBe(
      "this document is a list rather than an object, so it declares no nodes",
    );
    expect(readFlow('"orders"').edges.absence).toBe(
      "this document is a string rather than an object, so it declares no edges",
    );
    expect(readFlow("null").nodes.absence).toBe(
      "this document is null rather than an object, so it declares no nodes",
    );
  });

  it("counts the entries it cannot read instead of dropping them in silence", () => {
    const shape = readFlow(
      `{
         "nodes": [{ "id": "ingress" }, { "type": "respond" }, "parse-order"],
         "edges": [{ "from": "ingress", "to": "respond" }, { "from": "ingress" }]
       }`,
    );

    expect(shape.nodes.items.map((node) => node.id)).toEqual(["ingress"]);
    expect(shape.nodes.unreadable).toBe(2);
    expect(shape.edges.items).toHaveLength(1);
    expect(shape.edges.unreadable).toBe(1);
    // a node's own position is the document's, so the entries it skipped are
    // never renumbered away
    expect(shape.nodes.items[0].position).toBe(1);
  });

  it("answers rather than throwing on bytes that are not JSON at all", () => {
    const shape = readFlow('{ "nodes"');

    expect(shape.nodes.items).toEqual([]);
    expect(shape.nodes.absence).toBe("these bytes are not valid JSON, so they declare no nodes");
  });
});

describe("DraftStructure", () => {
  it("draws §2.4's node table, with every absent field said in words", () => {
    const { container } = render(() => <DraftStructure definition={parsingDraft.definition} />);

    expect(container.querySelector("caption")).toHaveTextContent(
      "the nodes this draft's document declares",
    );
    const table = rows(container);
    expect(table).toHaveLength(6);
    // the toggle sits in the id cell, so the id is read off the row's own cells
    expect(table[0][0]).toContain("ingress");
    expect(table[0][1]).toBe("request");
    // no fixture node carries a connection, and the cell says so rather than blanking
    expect(table[0][2]).toBe("—no connection recorded");
    expect(table[0][3]).toBe("—no config recorded");
    // a config the reader has not opened is summarised, not printed
    expect(table[1][3]).toBe("{ 2 keys }");
    expect(container.querySelector("tfoot")).toHaveTextContent(
      "— marks a field this document does not carry",
    );
  });

  it("opens one node's config, and offers nothing to open on a node with none", () => {
    const { container, getByLabelText, queryByLabelText } = render(() => (
      <DraftStructure definition={parsingDraft.definition} />
    ));

    // ingress carries no config field, so there is nothing to open on its row
    expect(queryByLabelText("expand node ingress")).toBeNull();

    fireEvent.click(getByLabelText("expand node parse-order"));
    flush();

    const opened = container.querySelector(".data-table-disclosed");
    expect(opened?.querySelector(".json-view")).toBeInTheDocument();
    expect(opened).toHaveTextContent("body.order");
    // the row and the document inside it are named for two different things
    getByLabelText("copy node parse-order config");
  });

  it("keeps a reader's open row on the node they opened when the document is re-read", () => {
    const withIngress = `{
      "nodes": [
        { "id": "ingress", "type": "request" },
        { "id": "reserve", "type": "integrate", "config": { "warehouse": "eu-1" } }
      ],
      "edges": []
    }`;
    const withoutIngress = `{
      "nodes": [{ "id": "reserve", "type": "integrate", "config": { "warehouse": "eu-1" } }],
      "edges": []
    }`;
    const [definition, setDefinition] = createSignal(withIngress);
    const { container, getByLabelText } = render(() => (
      <DraftStructure definition={definition()} />
    ));

    fireEvent.click(getByLabelText("expand node reserve"));
    flush();

    // §2.6's refresh re-issues the read, and a row keyed by its position would
    // hand the reader's open row to whichever node lands at that position — a
    // node they never opened, standing open over evidence they never asked for
    setDefinition(withoutIngress);
    flush();

    getByLabelText("collapse node reserve");
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(1);
    expect(container.querySelector(".data-table-disclosed")).toHaveTextContent("eu-1");
  });

  it("keeps two nodes that name one id as two rows, and names their controls apart", () => {
    const { container, getByLabelText } = render(() => (
      <DraftStructure
        definition={`{
          "nodes": [
            { "id": "twin", "type": "transform", "config": { "n": 1 } },
            { "id": "twin", "type": "transform", "config": { "n": 2 } }
          ],
          "edges": []
        }`}
      />
    ));

    // a half-finished draft may name one id twice, and two rows that collapsed
    // into one would hide a node the document declares
    expect(rows(container)).toHaveLength(2);
    // §3's floor: two controls of one screen may not share a name, so the
    // document's own position tells the two rows apart
    getByLabelText("expand node twin at position 1");
    getByLabelText("expand node twin at position 2");

    fireEvent.click(getByLabelText("expand node twin at position 2"));
    flush();
    expect(container.querySelector(".data-table-disclosed")).toHaveTextContent("2");
    getByLabelText("copy node twin at position 2 config");
  });

  it("draws the edges as rows grouped under their source node", () => {
    const { container } = render(() => <DraftStructure definition={platformDocument} />);

    expect(groupHeadings(container)).toEqual(["from ingress", "from check-stock"]);
    expect(edgeRows(container)).toEqual([
      "ingress[(main)] ─→ check-stock[(default input)]",
      "check-stock[backorder] ─→ reserve[(default input)]",
      "check-stock[backorder] ─→ fetch-inventory[(default input)]",
      "check-stock[in-stock] ─→ reserve[main]",
    ]);
  });

  it("shows what the platform reads at an end whose port the document does not name", () => {
    const { container } = render(() => <DraftStructure definition={platformDocument} />);

    // never "no port recorded": an unnamed `from-port` is `main` and an unnamed
    // `to-port` is the node's default input, so both ends show the value the
    // engine will use, marked as one the document does not contain
    expect(container).not.toHaveTextContent("no port recorded");
    expect(spoken(container)).toContain("no from-port named — main, the platform's default");
    expect(spoken(container)).toContain("no to-port named — the target node's default input");
    // and the parentheses are explained once, so the shape is never the only carrier
    expect(laneNotes(container)).toContain(
      "a port in parentheses is one this document does not name: the platform reads an unnamed from-port as main, and an unnamed to-port as the target node's default input",
    );
  });

  it("says where the order inside a fan-out came from", () => {
    const mixed = render(() => <DraftStructure definition={platformDocument} />);
    expect(laneNotes(mixed.container)).toContain(
      "within each port, 2 of 4 edges stand in the order their own ordinal names and the rest take their position in the group, as the platform does at parse",
    );
    cleanup();

    const none = render(() => (
      <DraftStructure
        definition={`{ "edges": [{ "from": "a", "to": "b" }, { "from": "a", "to": "c" }] }`}
      />
    ));
    expect(laneNotes(none.container)).toContain(
      "no edge names an ordinal, so within each port these stand in document order — the order the platform materializes at parse",
    );
    cleanup();

    const all = render(() => (
      <DraftStructure
        definition={`{ "edges": [
             { "from": "a", "to": "b", "ordinal": 0 },
             { "from": "a", "to": "c", "ordinal": 1 }
           ] }`}
      />
    ));
    expect(laneNotes(all.container)).toContain(
      "within each port, edges stand in the order their own ordinal names",
    );
  });

  it("says nothing about an order where no port dispatches twice", () => {
    // one edge per port declares no sequence, so a sentence about sequence would
    // be the screen filling silence rather than reading the document
    const { container } = render(() => (
      <DraftStructure
        definition={`{ "edges": [
             { "from": "a", "from-port": "left", "to": "b" },
             { "from": "a", "from-port": "right", "to": "c" }
           ] }`}
      />
    ));

    expect(laneNotes(container).join(" ")).not.toContain("ordinal");
  });

  it("shows a port named as the empty string as the port it is", () => {
    const { container } = render(() => (
      <DraftStructure
        definition={`{ "edges": [{ "from": "a", "from-port": "", "to": "b", "to-port": "" }] }`}
      />
    ));

    // `""` is a port this document names, and naming one is not the same fact as
    // naming none: the row may not fall back to the platform's default here
    expect(edgeRows(container)).toEqual([`a[""] ─→ b[""]`]);
    expect(container).not.toHaveTextContent("(main)");
    expect(spoken(container)).toEqual([
      "this document names this port as the empty string",
      "to",
      "this document names this port as the empty string",
    ]);
  });

  it("draws each end of §2.4's own draft on the port the document names", () => {
    // The branch ports are the whole point of the graph: `check-stock` leaves
    // by `in-stock` or by `backorder`, and an edge that named neither would say
    // the conditional had no branches. Every other end here names no port, so
    // it stands on the platform's default rather than on an invented name.
    // (A document spelling its ports with keys `Edge` refuses is pinned
    // separately, over a literal, by "reads no port out of a key the flow
    // document does not define".)
    const { container } = render(() => <DraftStructure definition={parsingDraft.definition} />);

    expect(edgeRows(container)).toEqual([
      "ingress[(main)] ─→ parse-order[(default input)]",
      "parse-order[(main)] ─→ check-stock[(default input)]",
      "check-stock[in-stock] ─→ reserve[(default input)]",
      "check-stock[backorder] ─→ fetch-inventory[(default input)]",
      "fetch-inventory[(main)] ─→ reserve[(default input)]",
      "reserve[(main)] ─→ respond[(default input)]",
    ]);
  });

  it("stands on a document that parses and is not a flow, and states both absences", () => {
    const { container } = render(() => (
      <DraftStructure definition={`{ "name": "half-finished", "version": 3 }`} />
    ));

    const states = [...container.querySelectorAll(".empty-state")].map(
      (state) => state.textContent ?? "",
    );
    expect(states).toHaveLength(2);
    expect(states[0]).toContain(`this document carries no "nodes" field`);
    expect(states[1]).toContain(`this document carries no "edges" field`);
    // an empty list of nodes is not a table of blank rows
    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(0);
    expect(container.querySelectorAll(".draft-edge")).toHaveLength(0);
  });

  it("separates an empty list from a missing one", () => {
    const { container } = render(() => (
      <DraftStructure definition={`{ "nodes": [], "edges": [] }`} />
    ));

    expect(container).toHaveTextContent(`this document's "nodes" list is empty`);
    expect(container).toHaveTextContent(`this document's "edges" list is empty`);
    // there are no edges to order and none whose ports could go unnamed
    expect(laneNotes(container)).toEqual([]);
  });

  it("subtracts the entries it could not read in words, under the lane they came from", () => {
    const { container } = render(() => (
      <DraftStructure
        definition={`{ "nodes": [{ "id": "ingress" }, { "type": "respond" }], "edges": [{ "from": "ingress" }] }`}
      />
    ));

    expect(container.querySelector("tfoot")).toHaveTextContent(
      `one entry of "nodes" has no id, so it is not listed`,
    );
    expect(container.querySelector(".draft-structure-note")).toHaveTextContent(
      `one entry of "edges" has no from or to, so it is not listed`,
    );
  });

  it("draws the charge flow the uncertain runs execute, three nodes and two edges", () => {
    const { container } = render(() => (
      <DraftStructure definition={unattributedDraft.definition} />
    ));

    expect(rows(container)).toHaveLength(3);
    expect(edgeRows(container)).toEqual([
      "ingress[(main)] ─→ charge-card[(default input)]",
      "charge-card[(main)] ─→ respond[(default input)]",
    ]);
  });

  it("draws the notifications draft's fan-out in the order the engine will dispatch it", () => {
    const { container } = render(() => <DraftStructure definition={loopingDraft.definition} />);

    expect(groupHeadings(container)).toEqual([
      "from ingress",
      "from notify-subscriber",
      "from next-subscriber",
    ]);
    // `record-delivery` is the *third* edge of the stored document and the
    // *first* row of its group: the draft gives the two edges leaving
    // `notify-subscriber` the ordinals 1 and 0 in that order, and the screen
    // draws the order the engine will dispatch rather than the order the array
    // holds. An author debugging this branch is reading their own `ordinal`s.
    expect(edgeRows(container)).toEqual([
      "ingress[(main)] ─→ notify-subscriber[(default input)]",
      "notify-subscriber[(main)] ─→ record-delivery[(default input)]",
      "notify-subscriber[(main)] ─→ next-subscriber[(default input)]",
      "next-subscriber[more] ─→ notify-subscriber[(default input)]",
    ]);
    // and the lane says which of the two the reader is looking at
    expect(laneNotes(container)).toContain(
      "within each port, 2 of 4 edges stand in the order their own ordinal names and the rest take their position in the group, as the platform does at parse",
    );
  });
});
