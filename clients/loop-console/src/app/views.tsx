import type { JSX } from "@solidjs/web";

import { toHash, type Route } from "../routing/route";
import { parseRevision } from "../routing/revision";
import { DraftScreen } from "../screens/draft-screen";
import { ReportScreen } from "../screens/report-screen";
import { RunScreen } from "../screens/run-screen";

function Screen(props: { name: string; children?: JSX.Element }): JSX.Element {
  return (
    <section class="screen">
      <h1 class="screen-name">{props.name}</h1>
      {props.children}
    </section>
  );
}

function NotFound(props: { hash: string }): JSX.Element {
  return (
    <Screen name="not found">
      <p class="screen-params">hash {props.hash}</p>
    </Screen>
  );
}

/**
 * The mounted screens, and the step-1 placeholder for the one route step 8
 * still owns.
 *
 * A mounted screen brings its own heading: §1.3's verdict bar is the screen's
 * `h1`, so the placeholder's `screen-name` heading goes with the placeholder
 * rather than standing above the verdict as a second one (wamn-dggp.21).
 */
export function routeView(route: Route): JSX.Element {
  switch (route.kind) {
    case "start":
      return <Screen name="start" />;
    case "run":
      return <RunScreen id={route.id} />;
    case "report":
      return <ReportScreen id={route.id} />;
    case "draft": {
      // The route carries the revision opaquely; this is where it becomes the
      // number the reader takes. A segment that cannot name a revision names no
      // draft either, so it is not-found — never a read that failed.
      const revision = parseRevision(route.revision);
      // Identity, not truthiness: revision 0 is a revision.
      if (revision === null) {
        return <NotFound hash={toHash(route)} />;
      }
      return <DraftScreen id={route.id} revision={revision} />;
    }
    case "not-found":
      return <NotFound hash={route.hash} />;
  }
}
