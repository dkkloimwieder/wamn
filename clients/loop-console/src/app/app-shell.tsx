import type { JSX } from "@solidjs/web";

import { CommandPalette } from "../palette/palette";
import type { Route } from "../routing/route";
import { TopBar } from "./top-bar";
import { routeView } from "./views";

/** Spec §1.4: top bar, then the 260px side panel beside the 88ch column. */
export function AppShell(props: { route: Route }): JSX.Element {
  return (
    <div class="shell">
      <TopBar />
      <div class="shell-body">
        {/* Step 6 fills the panel with the nav tree. */}
        <aside class="side-panel" aria-label="side panel" />
        {/*
         * `tabindex="-1"`, so the palette can hand a keyboard reader the column
         * after a ⌘K navigation. The screen they were standing on is unmounted
         * by that navigation, so there is no control left to restore focus to;
         * the column outlives the swap and is the head of what they asked for.
         * -1 keeps it out of the tab order — it is a landing place, not a stop.
         */}
        <main class="column" tabindex="-1">{routeView(props.route)}</main>
      </div>
      {/*
       * Outside the column and last, so ⌘K is heard on every route and the
       * overlay is drawn over the whole shell rather than inside 88ch. It
       * renders nothing at all until it is opened (§6 step 7).
       */}
      <CommandPalette />
    </div>
  );
}
