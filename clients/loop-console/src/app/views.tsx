import type { JSX } from "@solidjs/web";

import type { Route } from "../routing/route";

function Screen(props: { name: string; children?: JSX.Element }): JSX.Element {
  return (
    <section class="screen">
      <h1 class="screen-name">{props.name}</h1>
      {props.children}
    </section>
  );
}

/** Step-1 placeholders: each screen names itself and echoes its decoded params. */
export function routeView(route: Route): JSX.Element {
  switch (route.kind) {
    case "start":
      return <Screen name="start" />;
    case "run":
      return (
        <Screen name="run">
          <p class="screen-params">id {route.id}</p>
        </Screen>
      );
    case "report":
      return (
        <Screen name="report">
          <p class="screen-params">id {route.id}</p>
        </Screen>
      );
    case "draft":
      return (
        <Screen name="draft">
          <p class="screen-params">
            id {route.id} · revision {route.revision}
          </p>
        </Screen>
      );
    case "not-found":
      return (
        <Screen name="not found">
          <p class="screen-params">hash {route.hash}</p>
        </Screen>
      );
  }
}
