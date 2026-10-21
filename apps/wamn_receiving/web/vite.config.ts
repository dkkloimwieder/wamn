import { fileURLToPath } from "node:url";
import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";
import { applicationConfig } from "../../../web/shell/vite";

export default defineConfig(({ command, mode }) => {
  const application = applicationConfig({
    root: import.meta.url,
    client: { name: "@wamn/receiving-client", path: "../../target/wamn/wamn_receiving/client-ts" },
    outDir: "../../target/wamn/wamn_receiving/web",
    port: 5182,
    command: mode === "test" ? "build" : command,
  });
  return {
    ...application,
    plugins: [
      solid({ performanceTracks: false }),
      ...application.plugins,
      {
        name: "receiving-solid-version",
        generateBundle(_options, bundle) {
          const modules = Object.values(bundle).flatMap((item) =>
            item.type === "chunk" ? Object.keys(item.modules) : [],
          );
          const solid = modules.filter((id) => /\/solid-js\//.test(id));
          if (
            !solid.length ||
            solid.some((id) => !id.includes("solid-js@2.0.0-rc.16/")) ||
            modules.some((id) => /\/web\/(shell|ui|components)\//.test(id))
          ) {
            this.error("Receiving must bundle Solid 2.0.0-rc.16 and the wfe UI only");
          }
        },
      },
    ],
    resolve: {
      conditions: ["wfe-source", "browser", "development|production"],
      dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals"],
      alias: [
        ...application.resolve.alias.filter(
          ({ find }) => String(find).includes("client") || find === "@wamn/web-runtime",
        ),
        {
          find: "@wamn/platform-ui",
          replacement: fileURLToPath(new URL("../../platform_ui/src/index.ts", import.meta.url)),
        },
      ],
    },
    test: { environment: "jsdom", server: { deps: { inline: true } } },
  };
});
