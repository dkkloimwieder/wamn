import { solidApplication } from "@wamn/vite";
import { configDefaults, defineConfig } from "vitest/config";

const solid = solidApplication("platform");
const bundle = "test/bundle.test.ts";

// The bundle test runs a Vite build in its worker. It runs after the UI tests,
// so the build never takes the CPU that their findBy waits need.
export default defineConfig({
  ...solid,
  test: {
    ...solid.test,
    testTimeout: 15_000,
    projects: [
      {
        extends: true,
        test: { name: "ui", exclude: [...configDefaults.exclude, bundle], sequence: { groupOrder: 0 } },
      },
      { extends: true, test: { name: "bundle", include: [bundle], sequence: { groupOrder: 1 } } },
    ],
  },
});
