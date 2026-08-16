/*
 * No `@testing-library/jest-dom` here, unlike the jsdom setup: `@vitest/browser`
 * ships the same matcher set natively, and registering both would install two
 * implementations of one name.
 */

/**
 * The console's two global stylesheets, which `main.tsx` is otherwise the only
 * importer of.
 *
 * A browser test that did not load these would render against no tokens and no
 * shell rules — it would run in a real browser and still prove nothing about
 * what the console looks like, which is the one thing this project exists for.
 * Loading them here rather than per-file means a proof cannot silently be
 * written against a page that never had the stylesheet under test.
 */
import "./src/styles/tokens.css";
import "./src/styles/app.css";
