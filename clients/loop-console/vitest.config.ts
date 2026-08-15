import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";

/*
 * Test level, per the D2 option-B decision and its zero-new-dependency rule:
 * Solid's SSR build in a node environment. `renderToString` proves rendered
 * markup — every state a component can be asked to render — but the reactive
 * graph is inert here and there is no DOM, so no event, focus, or toggle
 * behaviour can be asserted at this level.
 *
 * Consequence, open at step 1's close: the behavioural done-checks of steps 3
 * (each primitive renders every state, focusable), 6 (mouse-driven navigation
 * end to end, roving focus) and 7 (every action reachable via the palette)
 * need an owner decision before step 3 starts — either a dependency-policy
 * amendment adding a DOM environment (jsdom/happy-dom) or Vitest browser
 * mode, or an explicit record that those checks are demonstrated manually.
 */
export default defineConfig({
  plugins: [solid({ ssr: true })],
  resolve: { conditions: ["solid"] },
  test: { environment: "node" },
});
