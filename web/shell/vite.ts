/**
 * The Vite configuration every application in `apps/<app>/web/` shares.
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
import type { ServerResponse } from "node:http";
import { fileURLToPath } from "node:url";

/** One proxied path. Vite reads the same shape. */
interface Proxy {
  readonly target: string;
  readonly secure?: boolean;
  readonly changeOrigin?: boolean;
  readonly headers?: Record<string, string>;
  readonly rewrite?: (path: string) => string;
}

/** A dev server plugin. Vite reads the same shape. */
interface DevPlugin {
  readonly name: string;
  readonly configureServer: (server: {
    readonly middlewares: {
      use(path: string, handle: (request: unknown, response: ServerResponse) => void): unknown;
    };
  }) => void;
}

/**
 * The part of a Vite configuration this module writes. It states the shape
 * itself, because the shell and each application install their own Vite.
 */
export interface ApplicationConfig {
  /** The dev server answers `/config.json` from `dev.json`. A build has none. */
  readonly plugins: DevPlugin[];
  readonly resolve: {
    readonly alias: { find: string | RegExp; replacement: string }[];
    readonly dedupe: string[];
  };
  readonly server: {
    readonly port: number;
    readonly proxy?: Record<string, Proxy>;
    readonly fs: { readonly allow: string[] };
  };
  /** The build writes into the package's build output directory, which it owns. */
  readonly build: { readonly outDir: string; readonly emptyOutDir: boolean };
}

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
function proxy(parsed: DevConfiguration): Record<string, Proxy> {
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
function scopePlugin(dev: DevConfiguration): DevPlugin {
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
 * The resolution and the server of one application. The application adds its
 * own plugins, because it installs them.
 */
export function applicationConfig(options: ApplicationOptions): ApplicationConfig {
  const at = (path: string) => fileURLToPath(new URL(path, options.root));
  const web = fileURLToPath(new URL("..", import.meta.url));
  const store = fileURLToPath(new URL("../../node_modules", import.meta.url));
  const dev = options.command === "serve" ? devConfiguration() : undefined;
  return {
    plugins: dev === undefined ? [] : [scopePlugin(dev)],
    // The runtime lives outside the application.
    resolve: {
      alias: [{ find: "@wamn/web-runtime", replacement: `${web}runtime/src/index.ts` }],
      dedupe: [],
    },
    server: {
      port: options.port,
      ...(dev === undefined ? {} : { proxy: proxy(dev) }),
      // Allow shared sources and pnpm packages outside the application.
      fs: { allow: [at("."), web, store] },
    },
    build: { outDir: at(options.outDir), emptyOutDir: true },
  };
}

/** One output file of a bundle. Vite reads the same shape. */
interface BundleItem {
  readonly type: string;
  code?: string;
}

/** A build plugin. Vite reads the same shape. */
interface BuildPlugin {
  readonly name: string;
  readonly enforce: "post";
  readonly generateBundle: (options: unknown, bundle: Record<string, BundleItem>) => void;
}

/**
 * The @wfe/sync worker recipe: `sw.js` precaches every other bundle file and
 * the named files that Vite does not bundle, such as public assets.
 */
export function precacheManifest(extra: readonly string[]): BuildPlugin {
  return {
    name: "precache-manifest",
    enforce: "post",
    generateBundle(_options, bundle) {
      const chunk = bundle["sw.js"];
      if (chunk?.type !== "chunk" || chunk.code === undefined) throw new Error("sw.js was not built");
      const files = Object.keys(bundle).filter((name) => name !== "sw.js");
      chunk.code = chunk.code.replace("__PRECACHE_MANIFEST__", JSON.stringify(["./", ...files, ...extra]));
    },
  };
}
