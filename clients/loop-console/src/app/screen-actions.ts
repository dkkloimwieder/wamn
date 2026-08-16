import { createSignal, onCleanup } from "solid-js";

/**
 * What the mounted screen offers the palette's `this screen` group (§6 step 7):
 * its section anchors, and the view actions that would otherwise be reachable
 * only by finding a control on the page.
 *
 * This is a seam and not a duplicate implementation. An action here *is* the
 * screen's own callback — the same one its visible control calls — so the
 * palette can never drift into a second, differently-behaved `expand all`.
 * §6 step 7 is explicit that there are no direct keys anywhere else, which makes
 * the palette the keyboard's only route to these and puts the burden on the
 * screen to offer every one of them.
 */

export type ScreenAnchor = {
  /** The DOM id of the section, which is what `focusAnchor` moves the reader to. */
  readonly id: string;
  readonly label: string;
  /**
   * Why this section cannot be reached, when a screen has one to name — §2.0's
   * `structure` "dimmed with a tooltip when the draft doesn't parse".
   *
   * Optional and additive on purpose: every reader of this type predates it,
   * and a section that *can* be reached says nothing rather than saying "not
   * unavailable". The side panel draws such a row dimmed with the reason as its
   * tooltip and refuses to activate it; a caller that ignores the field is left
   * exactly where it was.
   */
  readonly unavailable?: string;
};

export type ScreenAction = {
  /** Stable across re-renders, so the palette's list does not reshuffle under the cursor. */
  readonly id: string;
  readonly label: string;
  readonly run: () => void;
};

export type ScreenContribution = {
  readonly anchors: readonly ScreenAnchor[];
  readonly actions: readonly ScreenAction[];
};

const empty: ScreenContribution = { anchors: [], actions: [] };
const emptyThunk = (): ScreenContribution => empty;

/**
 * A thunk rather than a value, because a screen's offer is not fixed: a draft
 * grows its `structure` anchor only once the read says it parses, and a run's
 * failure section is not there at all when it did not fail. Holding the getter
 * lets the palette read the live answer at the moment it opens instead of
 * whatever was true when the screen mounted.
 *
 * The thunk is a plain variable and the signal beside it only announces that it
 * changed. A function cannot live in a Solid 2 signal at all: `createSignal(fn)`
 * builds a writable *memo* that calls `fn` and stores the result, so a getter
 * put there would be invoked once and replaced by its own answer.
 *
 * `ownedWrite` for the reason the visited store uses it — the writer is always a
 * screen contributing from inside its own owned scope, which is what this option
 * exists to license.
 */
let contribute: () => ScreenContribution = emptyThunk;
const [changed, setChanged] = createSignal(0, { ownedWrite: true });
let revision = 0;

function announce(): void {
  revision += 1;
  setChanged(revision);
}

/**
 * Called by a screen in its body. Releases on unmount, so a palette opened after
 * navigating away offers the new screen's sections and never the old screen's —
 * which would otherwise scroll to an anchor that is no longer on the page.
 */
export function contributeScreen(get: () => ScreenContribution): void {
  contribute = get;
  announce();
  onCleanup(() => {
    // Only if this screen is still the one contributing. A route change can
    // mount the next screen before the previous one disposes, and an
    // unconditional reset would then erase the offer of the screen that is
    // actually on the page.
    if (contribute === get) {
      contribute = emptyThunk;
      announce();
    }
  });
}

/** The mounted screen's live offer; empty when no screen contributes one. */
export function screenContribution(): ScreenContribution {
  changed();
  return contribute();
}

/**
 * Move the reader to a section.
 *
 * Focus, not only scroll: a palette that scrolled the page and left focus in a
 * closed overlay would strand a keyboard reader at the top of the document.
 * Sections carry `tabindex="-1"` so they can receive it without entering the
 * tab order.
 *
 * `scrollIntoView` is called defensively because jsdom does not implement it at
 * all — an unguarded call throws there, which would make every anchor test a
 * test of the environment rather than of the behaviour.
 */
export function focusAnchor(id: string): boolean {
  const target = document.getElementById(id);
  if (target === null) {
    return false;
  }
  target.scrollIntoView?.();
  target.focus();
  return true;
}
