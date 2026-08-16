import solid from "@solidjs/vite-plugin";
import { playwright } from "@vitest/browser-playwright";
import { defineConfig } from "vitest/config";

/**
 * Two projects, one run (`vitest run` executes both).
 *
 * Client only — never SSR. Tests load the same build the console ships, so the
 * reactive graph is live and behaviour is assertable. Per
 * https://v2.solidjs.com/guides/testing.
 *
 * **unit** is the default level and where almost everything belongs: jsdom, no
 * browser to start, the whole suite in under a second. The Solid guide calls it
 * the right default and it is.
 *
 * **browser** exists for the assertions jsdom cannot make at all, which the
 * guide sends to browser mode and which this console has a lot of (§2.6 is
 * focus-heavy, §1 is a token system that only means something once it is
 * painted). Measured in jsdom 30.0.1, not assumed: `IntersectionObserver`,
 * `ResizeObserver`, `matchMedia` and `Element.scrollTo` are all undefined,
 * `getBoundingClientRect` answers every element with zeros, and no stylesheet is
 * ever parsed. §2.0's "the section in view is marked as you scroll" therefore
 * has neither an implementation nor a fallback that runs there.
 *
 * A test earns its place in `browser` by needing real layout, real CSS or real
 * focus. Everything else stays in `unit`, where it runs hundreds of times faster
 * — a browser proof that could have been a jsdom proof is a slower suite for no
 * more truth.
 */
export default defineConfig({
  test: {
    projects: [
      {
        plugins: [solid()],
        test: {
          name: "unit",
          environment: "jsdom",
          globals: false,
          setupFiles: ["./vitest-setup.ts"],
          include: ["src/**/*.test.{ts,tsx}"],
          // `*.browser.test.tsx` also ends in `.test.tsx`; without this the
          // browser suite would be run a second time in jsdom, where the whole
          // reason it exists is absent, and fail for the reason it was written.
          exclude: ["src/**/*.browser.test.{ts,tsx}"],
        },
      },
      {
        plugins: [solid()],
        test: {
          name: "browser",
          globals: false,
          setupFiles: ["./vitest-setup-browser.ts"],
          include: ["src/**/*.browser.test.{ts,tsx}"],
          browser: {
            enabled: true,
            // Never opens a window: the suite runs the same way on a developer's
            // machine and in a checkout with no display at all.
            headless: true,
            provider: playwright(),
            instances: [{ browser: "chromium" }],
          },
        },
      },
    ],
  },
});
