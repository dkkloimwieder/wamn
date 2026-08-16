import type { ScreenAnchor } from "../app/screen-actions";
import { relativeTime } from "../palette/commands";
import type { Route } from "../routing/route";
import { parseRevision } from "../routing/revision";
import { routeOfVisit, type VisitedEntry, type VisitedKind } from "../store/visited";
import { ellipsize } from "../ui/copy-id";
import type { VerdictState } from "../ui/status";

/**
 * §2.0's tree, as data.
 *
 * Everything that decides *which* rows stand, what each one says, and which one
 * the route is on lives here — so the panel's shape is testable without a
 * render, and `side-panel.tsx` is left owning only the interaction. This is the
 * same split `palette/commands.ts` makes for the same reason.
 *
 * Three levels, never more (§2.0 says so outright): kinds → visited entities →
 * the active entity's section anchors. A draft is not a fourth level: the store
 * already labels one `orders@17`, which *is* §6 step 6's "grouped under flow
 * name".
 */

export type TreeNodeType = "start" | "kind" | "entity" | "anchor";

export type TreeNode = {
  /** Stable across rebuilds, so roving focus does not slip when the clock ticks. */
  readonly key: string;
  readonly type: TreeNodeType;
  readonly level: 1 | 2 | 3;
  /** What the row draws — middle-ellipsized, or the rail's short form. */
  readonly label: string;
  /**
   * The whole text when `label` is not it, and null when they agree. §2.6's
   * "hover reveals the full id when truncated", and what a reader hears in
   * place of a shortened one.
   */
  readonly full: string | null;
  /** §2.0's glyph, on the rows that carry a verdict at all. */
  readonly verdict: VerdictState | null;
  /** §2.0's relative time, or what stands in its place. */
  readonly detail: string | null;
  /**
   * Something true of this row that its own text does not say — §2.0's tooltip
   * on a dimmed `structure`, and why a pinned entity carries no time. Drawn as
   * a tooltip *and* spoken, because a `title` alone reaches neither a keyboard
   * reader nor a touch one.
   */
  readonly hint: string | null;
  /** Why activating this row would do nothing; null when it acts. */
  readonly unavailable: string | null;
  /** The row the current route is on — §2.0's 2px `signal` edge and `panel` ground. */
  readonly active: boolean;
  /** Where activating navigates; null on rows that are not addresses. */
  readonly route: Route | null;
  /** The section id activating moves to; null on rows that are not sections. */
  readonly anchor: string | null;
  /** null on a leaf: a row with no children makes no disclosure claim. */
  readonly expanded: boolean | null;
  /** §2.0 persists a collapse for level 1 only, so only a kind row carries one. */
  readonly kind: VisitedKind | null;
  readonly children: readonly TreeNode[];
  /** §2.0's `nothing visited yet`, on a kind that has nothing under it. */
  readonly emptyNote: string | null;
};

export type TreeInput = {
  readonly route: Route;
  /** The store's own order — most recently visited first, already capped per kind. */
  readonly visited: readonly VisitedEntry[];
  /** The mounted screen's live offer (`app/screen-actions.ts`), in document order. */
  readonly anchors: readonly ScreenAnchor[];
  readonly closedKinds: readonly VisitedKind[];
  /** §2.0's 44px rail: kind initials and the active entity's glyph only. */
  readonly collapsed: boolean;
  /** Epoch ms the relative times are measured against. */
  readonly now: number;
};

/**
 * §2.0's three fixed kinds, in the order the spec draws them.
 *
 * The rail form is three letters rather than one because two of the three kinds
 * begin with the same letter: a rail marked `R`, `R`, `D` would put a reader in
 * front of two rows that look identical and mean different things, which is the
 * one thing a 44px panel must not do.
 */
const kinds: ReadonlyArray<{ kind: VisitedKind; label: string; rail: string }> = [
  { kind: "run", label: "RUNS", rail: "RUN" },
  { kind: "report", label: "REPORTS", rail: "RPT" },
  { kind: "draft", label: "DRAFTS", rail: "DFT" },
];

/** The entity a route addresses, or null for the routes that address none. */
export type ActiveEntity = {
  readonly kind: VisitedKind;
  readonly id: string;
  readonly revision: number | null;
};

export function activeEntity(route: Route): ActiveEntity | null {
  switch (route.kind) {
    case "run":
    case "report":
      return { kind: route.kind, id: route.id, revision: null };
    case "draft": {
      // The router carries the revision opaquely; a segment that names no
      // revision names no draft either, so there is no entity to pin.
      const revision = parseRevision(route.revision);
      return revision === null ? null : { kind: "draft", id: route.id, revision };
    }
    case "start":
    case "not-found":
      return null;
  }
}

function entityKey(kind: VisitedKind, id: string, revision: number | null): string {
  return `${kind}:${id}@${revision ?? ""}`;
}

/**
 * §2.0's "plus the current entity always pinned in place even if never stored".
 *
 * A read the platform refused records no visit — there is no entity to
 * remember — but the route is still the one the reader is on, and a tree that
 * dropped it would answer "you are nowhere". So the row is derived from the
 * address, and deliberately not written into the store: the store holds
 * entities that were *read*, and an id that answered with a refusal is not one.
 */
function pinnedEntry(active: ActiveEntity): VisitedEntry {
  return {
    kind: active.kind,
    id: active.id,
    revision: active.revision,
    label: active.revision === null ? active.id : `${active.id}@${active.revision}`,
    // Not a verdict this console read anywhere: `none` is §2.0's `•`, which is
    // the absence of one rather than a claim about the entity.
    verdict: "none",
    visitedAt: 0,
  };
}

/** A row's entity, and whether the store is where it came from. */
type PanelEntry = { readonly entry: VisitedEntry; readonly recorded: boolean };

function entriesOfKind(kind: VisitedKind, input: TreeInput): readonly PanelEntry[] {
  const active = activeEntity(input.route);
  const stored = input.visited
    .filter((entry) => entry.kind === kind)
    .map<PanelEntry>((entry) => ({ entry, recorded: true }));

  if (active === null || active.kind !== kind) {
    return stored;
  }
  const key = entityKey(active.kind, active.id, active.revision);
  if (stored.some((item) => entityKey(item.entry.kind, item.entry.id, item.entry.revision) === key)) {
    return stored;
  }
  // "Pinned in place": at the head of its kind, which is where a just-visited
  // entity would have landed had the read given the store anything to hold.
  return [{ entry: pinnedEntry(active), recorded: false }, ...stored];
}

/**
 * What the row draws for an entity, and what it says when that is not the whole
 * text.
 *
 * A run and a report are their own opaque id, so §2.6's middle-ellipsis is what
 * makes them fit. A draft's stored label is `flowId@revision` — a name an author
 * chose — and ellipsizing that would hide the very word they recognize, so only
 * the *unrecorded* draft, whose label is an opaque draft id, is shortened.
 */
function entityText(item: PanelEntry): { label: string; full: string | null } {
  const entry = item.entry;
  if (entry.kind === "draft") {
    if (item.recorded) {
      return { label: entry.label, full: null };
    }
    const label = `${ellipsize(entry.id)}@${entry.revision}`;
    return { label, full: label === entry.label ? null : entry.label };
  }
  const label = ellipsize(entry.label);
  return { label, full: label === entry.label ? null : entry.label };
}

/** Why a pinned row carries no time — the absence stated rather than left blank. */
const UNRECORDED_HINT =
  "this route recorded no visit, so there is no time to show — the panel pins it because it is where you are";

function anchorNodes(parentKey: string, anchors: readonly ScreenAnchor[]): readonly TreeNode[] {
  return anchors.map((anchor) => ({
    key: `${parentKey}/${anchor.id}`,
    type: "anchor" as const,
    level: 3 as const,
    label: anchor.label,
    full: null,
    verdict: null,
    detail: null,
    // §2.0's `structure` dimmed with a tooltip when the draft doesn't parse:
    // the reason is the screen's, said in the screen's own words.
    hint: anchor.unavailable ?? null,
    unavailable: anchor.unavailable ?? null,
    active: false,
    route: null,
    anchor: anchor.id,
    expanded: null,
    kind: null,
    children: [],
    emptyNote: null,
  }));
}

function entityNode(item: PanelEntry, input: TreeInput, active: boolean): TreeNode {
  const entry = item.entry;
  const key = entityKey(entry.kind, entry.id, entry.revision);
  const text = entityText(item);
  /*
   * §2.0: the sections are "shown expanded only for the active entity". They
   * are the mounted screen's own anchors, so no other row could carry them —
   * a screen that is not on the page has no sections to scroll to.
   *
   * The rail has no room for a third level at all, and §2.0 spends its 44px on
   * the kind initials and the active entity's glyph instead.
   */
  const children = active && !input.collapsed ? anchorNodes(key, input.anchors) : [];

  return {
    key,
    type: "entity",
    level: 2,
    // The rail keeps the glyph and drops the words: §2.0 gives 44px to "kind
    // initials and the active entity's glyph only".
    label: input.collapsed ? "" : text.label,
    full: input.collapsed ? entry.label : text.full,
    verdict: entry.verdict,
    detail: input.collapsed
      ? null
      : item.recorded
        ? relativeTime(entry.visitedAt, input.now)
        : "not recorded",
    hint: item.recorded ? null : UNRECORDED_HINT,
    unavailable: null,
    active,
    route: routeOfVisit(entry),
    anchor: null,
    expanded: children.length === 0 ? null : true,
    kind: null,
    children,
    emptyNote: null,
  };
}

function kindNode(
  spec: { kind: VisitedKind; label: string; rail: string },
  input: TreeInput,
): TreeNode {
  const active = activeEntity(input.route);
  const items = entriesOfKind(spec.kind, input).filter(
    // The rail shows one entity at most, and it is the one the route is on.
    (item) =>
      !input.collapsed ||
      (active !== null &&
        entityKey(active.kind, active.id, active.revision) ===
          entityKey(item.entry.kind, item.entry.id, item.entry.revision)),
  );
  const children = items.map((item) =>
    entityNode(
      item,
      input,
      active !== null &&
        entityKey(active.kind, active.id, active.revision) ===
          entityKey(item.entry.kind, item.entry.id, item.entry.revision),
    ),
  );
  const open = !input.closedKinds.includes(spec.kind);

  return {
    key: `kind:${spec.kind}`,
    type: "kind",
    level: 1,
    label: input.collapsed ? spec.rail : spec.label,
    full: input.collapsed ? spec.label.toLowerCase() : null,
    verdict: null,
    detail: null,
    hint: null,
    unavailable: null,
    active: false,
    route: null,
    anchor: null,
    // A kind with nothing under it is a leaf, not a collapsed disclosure: there
    // is no group to open, and claiming one would be a control that does nothing.
    expanded: children.length === 0 ? null : open,
    kind: spec.kind,
    children,
    // §2.0's "the panel teaches its own mechanic". The rail has no room for the
    // sentence and the kind initial already stands there, so it is left to the
    // open panel rather than truncated into meaninglessness.
    emptyNote: children.length === 0 && !input.collapsed ? "nothing visited yet" : null,
  };
}

function startNode(input: TreeInput): TreeNode {
  return {
    key: "start",
    type: "start",
    level: 1,
    // In the rail the diamond *is* the row; open, it is drawn as the row's mark
    // beside the word (§2.0's `◈ start`).
    label: input.collapsed ? "◈" : "start",
    full: input.collapsed ? "start" : null,
    verdict: null,
    detail: null,
    hint: null,
    unavailable: null,
    active: input.route.kind === "start",
    route: { kind: "start" },
    anchor: null,
    expanded: null,
    kind: null,
    children: [],
    emptyNote: null,
  };
}

/** §2.0's whole tree: `start`, then the three kinds, always all three. */
export function buildTree(input: TreeInput): readonly TreeNode[] {
  return [startNode(input), ...kinds.map((spec) => kindNode(spec, input))];
}

/**
 * The rows a reader can actually reach, in the order they are drawn — which is
 * the order `↑/↓` walk and the order roving focus is held in. A closed node's
 * children are not among them; they are still in the tree, because collapsing a
 * kind hides its entities rather than forgetting them.
 */
export function visibleRows(nodes: readonly TreeNode[]): readonly TreeNode[] {
  const rows: TreeNode[] = [];
  const walk = (list: readonly TreeNode[]): void => {
    for (const node of list) {
      rows.push(node);
      if (node.expanded !== false) {
        walk(node.children);
      }
    }
  };
  walk(nodes);
  return rows;
}

/** Child key → parent key, which is where `←` goes from a row it cannot collapse. */
export function parentKeys(nodes: readonly TreeNode[]): ReadonlyMap<string, string> {
  const parents = new Map<string, string>();
  const walk = (list: readonly TreeNode[], parent: string | null): void => {
    for (const node of list) {
      if (parent !== null) {
        parents.set(node.key, parent);
      }
      walk(node.children, node.key);
    }
  };
  walk(nodes, null);
  return parents;
}

/**
 * §2.0's "the section in view is marked as you scroll", as the decision it
 * really is — kept apart from the observer that feeds it because jsdom
 * implements no `IntersectionObserver` at all, and a rule that only a browser
 * can execute is a rule that is never tested at the level it can be.
 *
 * The topmost section touching the viewport wins: with two sections on screen
 * the one a reader has scrolled *to* is the upper one, and the lower one is
 * what is coming. When nothing intersects — mid-scroll between two sections, or
 * a viewport shorter than the gap between them — the previous mark stands
 * rather than clearing, because a tree that blanked its mark would be claiming
 * the reader is in none of the entity's sections, which is never what happened.
 * A previous mark that has left the screen's anchor list is dropped, since the
 * section it named is no longer on the page.
 */
export function sectionInView(
  order: readonly string[],
  visible: readonly string[],
  previous: string | null,
): string | null {
  const seen = new Set(visible);
  const first = order.find((id) => seen.has(id));
  if (first !== undefined) {
    return first;
  }
  return previous !== null && order.includes(previous) ? previous : null;
}
