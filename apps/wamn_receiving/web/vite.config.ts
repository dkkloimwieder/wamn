import { fileURLToPath } from "node:url";
import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";
import { applicationConfig, precacheManifest } from "../../../web/shell/vite";
import { vendorTransforms } from "../../../web/wfe/scripts/vendor-transforms.mjs";

export default defineConfig(({ command, mode }) => {
  const application = applicationConfig({
    root: import.meta.url,
    outDir: "../../target/wamn/wamn_receiving/web",
    port: 5182,
    command: mode === "test" ? "build" : command,
  });
  return {
    ...application,
    // The page, and the worker that keeps it and config.json for a reload without a network.
    build: {
      ...application.build,
      rollupOptions: {
        input: {
          main: fileURLToPath(new URL("index.html", import.meta.url)),
          sw: fileURLToPath(new URL("src/sw.ts", import.meta.url)),
        },
        output: {
          entryFileNames: (chunk: { name: string }) => (chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js"),
        },
      },
    },
    plugins: [
      // The wfe source carries a captured engine with standard decorators.
      vendorTransforms(),
      solid({ performanceTracks: false }),
      ...application.plugins,
      precacheManifest(["config.json"]),
      {
        name: "receiving-solid-version",
        generateBundle(_options, bundle) {
          const modules = Object.values(bundle).flatMap((item) =>
            item.type === "chunk" ? Object.keys(item.modules) : [],
          );
          const solid = modules.filter((id) => /\/solid-js\//.test(id));
          if (
            !solid.length ||
            solid.some((id) => !id.includes("solid-js@2.0.0-rc.18/")) ||
            modules.some((id) => /\/web\/(ui|components)\//.test(id))
          ) {
            this.error("Receiving must bundle Solid 2.0.0-rc.18 and the wfe UI only");
          }
        },
      },
    ],
    resolve: {
      conditions: ["browser", "development|production"],
      dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals"],
      alias: [
        ...application.resolve.alias,
        {
          find: "@wamn/platform-ui",
          replacement: fileURLToPath(new URL("../../platform_ui/src/index.ts", import.meta.url)),
        },
      ],
    },
    test: { environment: "jsdom", server: { deps: { inline: true } } },
  };
});
