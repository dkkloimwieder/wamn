import type { JSX } from "@solidjs/web";

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
        <main class="column">{routeView(props.route)}</main>
      </div>
    </div>
  );
}
