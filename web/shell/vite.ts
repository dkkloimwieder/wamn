/**
 * The Vite configuration every Solid application of the repository shares.
 *
 * The page serves one origin. The dev server carries `/password` to the
 * identity process and `/api` to the release, and strips `/api` on the way.
 * The edge proxy of a deployment does the same, so one rule holds in both.
 *
 * The application host routes by the `Host` header, which a browser cannot
 * set. The identity process signed its own certificate, which a browser does
 * not trust. Both facts stop at this proxy.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import solid from "@solidjs/vite-plugin";
import type { Plugin, ProxyOptions } from "vite";
import type { ViteUserConfig as UserConfig } from "vitest/config";

/** The one Solid release that every application bundles. */
const SOLID_VERSION = "2.0.0-rc.18";

/** The part of a Vite configuration that `applicationConfig` writes. */
export type ApplicationConfig = Required<Pick<UserConfig, "plugins" | "resolve" | "server" | "build">>;

/** What one application states about itself. */
export interface ApplicationOptions {
  /** `import.meta.url` of the application's `vite.config.ts`. */
  readonly root: string;
  /** The directory of the built files, `target/wamn/<package>/web`, relative to the root. */
  readonly outDir: string;
  /** The port of the dev server. */
  readonly port: number;
  /** `serve` for the dev server, `build` for static files. Only `serve` reads `dev.json`. A build reads nothing about the deployment. */
  readonly command: "serve" | "build";
}

interface DevConfiguration {
  readonly route_host: string;
  readonly session_identity: { readonly issuer: string };
  readonly org: string;
  readonly project: string;
}

/** The `dev.json` of the local stack, from the variable the dev loop documents. */
function devConfiguration(): DevConfiguration {
  const directory = process.env["WAMN_DEV_ENV_DIR"];
  if (directory === undefined) {
    throw new Error("set WAMN_DEV_ENV_DIR to the directory that holds dev.json");
  }
  return JSON.parse(readFileSync(`${directory}/dev.json`, "utf8")) as DevConfiguration;
}

/** The proxy to the local stack, from `dev.json` and the base URL the dev loop printed. */
function proxy(parsed: DevConfiguration): Record<string, ProxyOptions> {
  const route = process.env["WAMN_ROUTE_URL"];
  if (route === undefined) {
    throw new Error("set WAMN_ROUTE_URL to the base URL the dev loop printed");
  }
  return {
    // The issuer signed its own certificate, so this hop does not verify it.
    // Nothing outside the loopback address is reachable here.
    "/password": { target: parsed.session_identity.issuer, secure: false, changeOrigin: true },
    "/api": {
      target: route,
      headers: { host: parsed.route_host },
      rewrite: (path) => path.replace(/^\/api/, ""),
    },
  };
}

/**
 * The dev server answers `/config.json` with the org and the project of
 * `dev.json`, as `wamn web upload` writes the file beside `index.html`.
 */
function scopePlugin(dev: DevConfiguration): Plugin {
  return {
    name: "wamn-scope",
    configureServer(server) {
      server.middlewares.use("/config.json", (_request, response) => {
        response.setHeader("content-type", "application/json");
        response.end(JSON.stringify({ org: dev.org, project: dev.project }));
      });
    },
  };
}

/**
 * The build stops unless the bundle holds Solid `SOLID_VERSION` only, and
 * no module of the Solid 1 UI packages that `web/ui` and `web/components` held.
 */
function solidVersion(application: string): Plugin {
  return {
    name: "solid-version",
    generateBundle(_options, bundle) {
      const modules = Object.values(bundle).flatMap((item) => (item.type === "chunk" ? Object.keys(item.modules) : []));
      const solid = modules.filter((id) => /\/solid-js\//.test(id));
      if (solid.length === 0 || solid.some((id) => !id.includes(`solid-js@${SOLID_VERSION}/`))) {
        this.error(`The ${application} bundle must contain only Solid ${SOLID_VERSION}: ${solid.join(", ")}`);
      }
      if (modules.some((id) => /\/web\/(ui|components)\//.test(id))) {
        this.error(`The ${application} bundle imports a Solid 1 UI package`);
      }
    },
  };
}

/**
 * The policy of every Solid application: the Solid compiler, the browser
 * resolution with one copy of Solid, the Solid version test and the test
 * environment. `dedupe` names further packages that must resolve once.
 */
export function solidApplication(
  application: string,
  dedupe: readonly string[] = [],
): Required<Pick<UserConfig, "plugins" | "resolve" | "test">> {
  return {
    plugins: [solid({ performanceTracks: false }), solidVersion(application)],
    resolve: {
      conditions: ["browser", "development|production"],
      dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals", ...dedupe],
    },
    test: { environment: "jsdom", server: { deps: { inline: true } } },
  };
}

/**
 * The server and the output of one application. The dev server alone adds
 * the scope response and the proxy to the local stack.
 */
export function applicationConfig(options: ApplicationOptions): ApplicationConfig {
  const at = (path: string) => fileURLToPath(new URL(path, options.root));
  const web = fileURLToPath(new URL("..", import.meta.url));
  const store = fileURLToPath(new URL("../../node_modules", import.meta.url));
  const dev = options.command === "serve" ? devConfiguration() : undefined;
  return {
    plugins: dev === undefined ? [] : [scopePlugin(dev)],
    resolve: {},
    server: {
      port: options.port,
      ...(dev === undefined ? {} : { proxy: proxy(dev) }),
      // Allow shared sources and pnpm packages outside the application.
      fs: { allow: [at("."), web, store] },
    },
    build: { outDir: at(options.outDir), emptyOutDir: true },
  };
}

/**
 * The inputs of a build with the @wfe/sync worker: the pages, and `worker`,
 * which the build writes as `sw.js` at the root of the output, so that its
 * scope covers every page. Every other entry goes to `assets/`.
 */
export function workerBuild(
  root: string,
  pages: Readonly<Record<string, string>>,
  worker: string,
): NonNullable<NonNullable<UserConfig["build"]>["rollupOptions"]> {
  const at = (path: string) => fileURLToPath(new URL(path, root));
  return {
    input: { ...Object.fromEntries(Object.entries(pages).map(([name, path]) => [name, at(path)])), sw: at(worker) },
    output: {
      entryFileNames: (chunk: { name: string }) => (chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js"),
    },
  };
}

/**
 * The @wfe/sync worker recipe: `sw.js` precaches every other bundle file and
 * the named files that Vite does not bundle, such as public assets.
 */
export function precacheManifest(extra: readonly string[]): Plugin {
  return {
    name: "precache-manifest",
    enforce: "post",
    generateBundle(_options, bundle) {
      const chunk = bundle["sw.js"];
      if (chunk?.type !== "chunk") throw new Error("sw.js was not built");
      const files = Object.keys(bundle).filter((name) => name !== "sw.js");
      chunk.code = chunk.code.replace("__PRECACHE_MANIFEST__", JSON.stringify(["./", ...files, ...extra]));
    },
  };
}
