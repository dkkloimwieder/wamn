/**
 * The page of the shell browser test: the shell over the two operations of
 * the host route fixture release (`crates/execution/host/tests/support/application_fixture.rs`).
 *
 * `tests/integration/src/shell_browser_live.rs` serves the hosts and the
 * identity stand-in, and `journey.mjs` drives this page in Chrome.
 */

import { render } from "solid-js/web";

import { ColorModeProvider } from "@wamn/ui";

import { Shell, type ShellSection } from "../../src/index.js";

const SECTIONS: readonly ShellSection[] = [
  {
    label: "Purchases",
    screens: [
      {
        path: "purchases",
        operation: "session-test:purchase/read@1.0.0",
        label: "read",
        component: () => <p>purchase read screen</p>,
      },
      {
        path: "purchases/new",
        operation: "session-test:purchase/write@1.0.0",
        label: "write",
        component: () => <p>purchase write screen</p>,
      },
    ],
  },
];

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no root element");
}
render(
  () => (
    <ColorModeProvider initialColorMode="light">
      <Shell
        title="Browser test"
        org={import.meta.env.WAMN_ORG}
        project={import.meta.env.WAMN_PROJECT}
        sections={SECTIONS}
      />
    </ColorModeProvider>
  ),
  root,
);
