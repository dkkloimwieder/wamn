/**
 * The demo serves one origin, and this proxy reaches the two the local stack
 * really has.
 *
 * The application host serves plain HTTP and routes by the `Host` header, which
 * a browser cannot set. The identity process serves HTTPS with a certificate it
 * signed itself, which a browser does not trust. Both facts stop at this proxy,
 * so the page calls same-origin paths and the generated code stays unchanged.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import solid from "vite-plugin-solid";
import { defineConfig, type ProxyOptions } from "vite";

/** The four models of the Receiving release, which are its path prefixes. */
const MODELS = ["/purchase_order", "/receipt", "/receiving", "/location"];

/** One path inside this package, as an absolute path. */
function local(path: string): string {
  return fileURLToPath(new URL(path, import.meta.url));
}

interface DevConfiguration {
  readonly route_host: string;
  readonly session_identity: { readonly issuer: string };
}

function environment(): { issuer: string; routeHost: string; route: string } {
  const directory = process.env["WAMN_DEV_ENV_DIR"];
  if (directory === undefined) {
    throw new Error("set WAMN_DEV_ENV_DIR to the directory that holds dev.json");
  }
  const route = process.env["WAMN_DEMO_ROUTE_URL"];
  if (route === undefined) {
    throw new Error("set WAMN_DEMO_ROUTE_URL to the base URL the dev loop printed");
  }
  const parsed = JSON.parse(readFileSync(`${directory}/dev.json`, "utf8")) as DevConfiguration;
  return { issuer: parsed.session_identity.issuer, routeHost: parsed.route_host, route };
}

export default defineConfig(() => {
  const { issuer, routeHost, route } = environment();
  const proxy: Record<string, ProxyOptions> = {
    // The issuer signed its own certificate, so this hop does not verify it.
    // Nothing outside the loopback address is reachable here.
    "/password": { target: issuer, secure: false, changeOrigin: true },
  };
  for (const model of MODELS) {
    proxy[model] = { target: route, headers: { host: routeHost } };
  }
  return {
    plugins: [solid()],
    // The generated modules live outside this package, so the names they
    // import are resolved here. A bare import from a generated file would
    // otherwise walk up a directory tree that installs nothing.
    resolve: {
      alias: [
        { find: "@wamn/web-runtime", replacement: local("../runtime/src/index.ts") },
        { find: "@wamn/receiving-client", replacement: local("../../apps/wamn_receiving/generated/client-ts") },
        { find: /^solid-js$/, replacement: local("node_modules/solid-js") },
        { find: /^@tanstack\/solid-table$/, replacement: local("node_modules/@tanstack/solid-table") },
        { find: /^@tanstack\/solid-form$/, replacement: local("node_modules/@tanstack/solid-form") },
        { find: /^zod$/, replacement: local("node_modules/zod") },
      ],
    },
    server: { port: 5180, proxy },
  };
});
