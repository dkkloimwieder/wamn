import { fileURLToPath } from "node:url";
import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    solid({ performanceTracks: false }),
    {
      name: "platform-solid-version",
      generateBundle(_options, bundle) {
        const modules = Object.values(bundle).flatMap((item) =>
          item.type === "chunk" ? Object.keys(item.modules) : [],
        );
        const solid = modules.filter((id) => /\/solid-js\//.test(id));
        if (solid.length === 0 || solid.some((id) => !id.includes("solid-js@2.0.0-rc.14/"))) {
          this.error(`The platform bundle must contain only Solid 2.0.0-rc.14: ${solid.join(", ")}`);
        }
        if (modules.some((id) => /\/web\/(shell|ui|components)\//.test(id))) {
          this.error("The platform bundle imports a Solid 1 UI package");
        }
        this.info("Platform bundle: Solid 2.0.0-rc.14 only");
      },
    },
  ],
  resolve: {
    conditions: ["wfe-source", "browser", "development|production"],
    dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals"],
    alias: {
      "@wamn/web-runtime": fileURLToPath(new URL("../../web/runtime/src/index.ts", import.meta.url)),
      "@wamn/control-client": fileURLToPath(new URL("../target/wamn/wamn_control/client-ts/index.ts", import.meta.url)),
      "@wamn/control-org-client": fileURLToPath(
        new URL("../target/wamn/wamn_control/control/client-ts/index.ts", import.meta.url),
      ),
    },
  },
  test: { environment: "jsdom", testTimeout: 15_000, server: { deps: { inline: true } } },
});
