import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vite";

import { applicationConfig } from "../../../web/shell/vite";

export default defineConfig(({ command }) => {
  const application = applicationConfig({
    root: import.meta.url,
    client: { name: "@wamn/receiving-client", path: "../generated/client-ts" },
    port: 5182,
    command,
  });
  return { ...application, plugins: [solid(), tailwindcss(), ...application.plugins] };
});
