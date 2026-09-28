// ESLint for the hand-written web code: the TypeScript recommended rules and
// the Solid rules. Generated output and build output are not linted.

import js from "@eslint/js";
import solid from "eslint-plugin-solid/configs/typescript";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "**/node_modules/**",
      "**/dist/**",
      "apps/*/generated/**",
      "web/components/fixture/**",
      "web/components/.vitest/**",
      "web/demo/**",
      "target/**",
      "apps/target/**",
    ],
  },
  {
    files: ["web/**/*.{ts,tsx}", "apps/*/web/**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended, solid],
    languageOptions: {
      globals: { ...globals.browser, ...globals.node },
    },
    rules: {
      // A leading underscore marks a name that is unused on purpose.
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", destructuredArrayIgnorePattern: "^_" },
      ],
      // Solid assigns a `ref={element}` variable, which this rule cannot see.
      "no-unassigned-vars": "off",
    },
  },
);
