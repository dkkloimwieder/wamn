import { createSignal } from "solid-js";

import { proxyTarget } from "../config";
import type { VisitedKind } from "../store/visited";

/**
 * What the panel remembers about itself: §2.0's 44px collapse and the level-1
 * kinds that are closed, both of which the spec says persist.
 *
 * Its own key, namespaced the way `store/visited.ts` namespaces its own — the
 * two are different things with different lifetimes, and `clear visited` must
 * not also throw away how a reader has arranged their panel.
 *
 * Storage is optional and every one of its calls can throw (disabled cookies,
 * private mode, a full quota). The panel is a convenience, so a browser that
 * refuses it gets a session-lived arrangement rather than a shell that will not
 * mount — the same trade, for the same reason, as the visited store's.
 */

export type PanelState = {
  readonly collapsed: boolean;
  /**
   * *Closed* rather than open, so a store this build has never written — and a
   * kind a later build adds — reads back as §2.0 draws it: every kind open.
   */
  readonly closedKinds: readonly VisitedKind[];
};

export const panelStorageKey = `wamn-loop-console/panel/${proxyTarget}`;

const kinds: ReadonlySet<string> = new Set<VisitedKind>(["run", "report", "draft"]);

export const initialPanelState: PanelState = { collapsed: false, closedKinds: [] };

/**
 * Storage is a foreign input: hand-editable, and older than this build. A shape
 * that is not recognized costs the reader their arrangement at worst, never a
 * panel that fails to draw — so each field is taken only when it is the shape
 * it must be, and the rest falls back to §2.0's own defaults.
 */
export function decodePanelState(raw: string | null): PanelState {
  if (raw === null) {
    return initialPanelState;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return initialPanelState;
  }
  if (typeof parsed !== "object" || parsed === null) {
    return initialPanelState;
  }
  const state = parsed as Record<string, unknown>;
  const closed = Array.isArray(state.closedKinds)
    ? state.closedKinds.filter((kind): kind is VisitedKind => typeof kind === "string" && kinds.has(kind))
    : [];
  return { collapsed: state.collapsed === true, closedKinds: closed };
}

export function encodePanelState(state: PanelState): string {
  return JSON.stringify(state);
}

function loadStored(): PanelState {
  try {
    return decodePanelState(window.localStorage.getItem(panelStorageKey));
  } catch {
    return initialPanelState;
  }
}

function persist(state: PanelState): void {
  try {
    window.localStorage.setItem(panelStorageKey, encodePanelState(state));
  } catch {
    // The panel still stands as arranged; only its survival across a reload is lost.
  }
}

/**
 * The value is this variable and the signal beside it only announces that it
 * moved — `store/visited.ts`'s shape, for its reason: a Solid 2 write is staged
 * until the graph flushes, so a toggle that read the signal back to compute its
 * next state would answer with the value it had just replaced.
 */
let current: PanelState = loadStored();
const [changed, setChanged] = createSignal(0);
let revision = 0;

function commit(next: PanelState): void {
  current = next;
  revision += 1;
  setChanged(revision);
  persist(next);
}

export function panelCollapsed(): boolean {
  changed();
  return current.collapsed;
}

export function closedKinds(): readonly VisitedKind[] {
  changed();
  return current.closedKinds;
}

/** §2.0's footer toggle, and the palette row wamn-dggp.34 adds beside it. */
export function togglePanelCollapsed(): void {
  commit({ ...current, collapsed: !current.collapsed });
}

export function setKindOpen(kind: VisitedKind, open: boolean): void {
  const closed = current.closedKinds.filter((closedKind) => closedKind !== kind);
  commit({ ...current, closedKinds: open ? closed : [...closed, kind] });
}

/** The arrangement §2.0 starts from — what a reader who has changed nothing sees. */
export function resetPanelState(): void {
  commit(initialPanelState);
}
