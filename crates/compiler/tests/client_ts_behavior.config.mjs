import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(new URL("../../../../web/runtime/package.json", import.meta.url));

export default {
  root: fileURLToPath(new URL("../../../../web/runtime/", import.meta.url)),
  resolve: {
    alias: {
      "@fixture": process.env.WAMN_CLIENT_TS_TEST_ROOT,
      "@wamn/web-runtime": fileURLToPath(new URL("../../../../web/runtime/src/index.ts", import.meta.url)),
      "@wfe/sync": require.resolve("@wfe/sync"),
    },
    conditions: ["browser", "development"],
  },
  test: {
    include: ["../../crates/schema/generator/tests/client_ts_behavior.test.ts"],
    environment: "node",
  },
};
