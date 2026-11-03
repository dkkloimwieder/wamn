import { applicationConfig, precacheManifest, solidApplication, workerBuild } from "@wamn/vite";
import { defineConfig } from "vitest/config";

export default defineConfig(({ command, mode }) => {
  const application = applicationConfig({
    root: import.meta.url,
    outDir: "../../target/wamn/wamn_receiving/web",
    port: 5182,
    command: mode === "test" ? "build" : command,
  });
  const solid = solidApplication("Receiving");
  return {
    ...application,
    ...solid,
    // The page, and the worker that keeps it and config.json for a reload without a network.
    build: { ...application.build, rollupOptions: workerBuild(import.meta.url, { main: "index.html" }, "src/sw.ts") },
    plugins: [...solid.plugins, ...application.plugins, precacheManifest(["config.json"])],
  };
});
