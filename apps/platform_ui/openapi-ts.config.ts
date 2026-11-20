import { readFileSync } from "node:fs";
import { defineConfig } from "@hey-api/openapi-ts";
import { wamn } from "./package.json";

const documents = new Map([
  ["wamn_control", "application.openapi.json"],
  ["wamn_control_org", "control.openapi.json"],
]);

export default defineConfig(
  wamn.clients.map((name) => {
    const document = documents.get(name);
    if (!document) throw new Error(`No OpenAPI document for ${name}`);
    return {
      input: JSON.parse(readFileSync(new URL(`../target/wamn/platform_ui/${document}`, import.meta.url), "utf8")),
      output: `src/generated/${name}`,
      plugins: [
        "@hey-api/typescript",
        "@hey-api/sdk",
        { name: "@hey-api/client-fetch", runtimeConfigPath: "./src/client-config" },
      ],
    };
  }),
);
