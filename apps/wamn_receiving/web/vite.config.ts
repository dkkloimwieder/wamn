import { applicationConfig, solidApplication } from "@wamn/vite";
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
    plugins: [...solid.plugins, ...application.plugins],
  };
});
