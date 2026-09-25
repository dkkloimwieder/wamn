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
import { fileURLToPath } from "node:url";

/** One proxied path. Vite reads the same shape. */
interface Proxy {
  readonly target: string;
  readonly secure?: boolean;
  readonly changeOrigin?: boolean;
  readonly headers?: Record<string, string>;
  readonly rewrite?: (path: string) => string;
}

/**
 * The part of a Vite configuration this module writes. It states the shape
 * itself, because the shell and each application install their own Vite.
 */
export interface ApplicationConfig {
  readonly resolve: {
    readonly alias: { find: string | RegExp; replacement: string }[];
    readonly dedupe: string[];
  };
  readonly server: {
    readonly port: number;
    readonly proxy?: Record<string, Proxy>;
    readonly fs: { readonly allow: string[] };
  };
}

/** What one application states about itself. */
export interface ApplicationOptions {
  /** `import.meta.url` of the application's `vite.config.ts`. */
  readonly root: string;
  /** The generated client: its package name and its directory, relative to the root. */
  readonly client: { readonly name: string; readonly path: string };
  /** The port of the dev server. */
  readonly port: number;
  /** `serve` for the dev server, `build` for static files. Only `serve` reads the environment. */
  readonly command: "serve" | "build";
}

interface DevConfiguration {
  readonly route_host: string;
  readonly session_identity: { readonly issuer: string };
}

/** The proxy to the local stack, from the variables the dev loop documents. */
function proxy(): Record<string, Proxy> {
  const directory = process.env["WAMN_DEV_ENV_DIR"];
  if (directory === undefined) {
    throw new Error("set WAMN_DEV_ENV_DIR to the directory that holds dev.json");
  }
  const route = process.env["WAMN_ROUTE_URL"];
  if (route === undefined) {
    throw new Error("set WAMN_ROUTE_URL to the base URL the dev loop printed");
  }
  const parsed = JSON.parse(readFileSync(`${directory}/dev.json`, "utf8")) as DevConfiguration;
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
 * The resolution and the server of one application. The application adds its
 * own plugins, because it installs them.
 */
export function applicationConfig(options: ApplicationOptions): ApplicationConfig {
  const at = (path: string) => fileURLToPath(new URL(path, options.root));
  const web = fileURLToPath(new URL("..", import.meta.url));
  const installed = (name: string) => at(`node_modules/${name}`);
  return {
    // The shell, the runtime, the UI and the generated client live outside the
    // application, so the names they import resolve here. A bare import from
    // one of them would otherwise walk up a directory tree that installs nothing.
    resolve: {
      alias: [
        { find: "@wamn/web-runtime", replacement: `${web}runtime/src/index.ts` },
        { find: "@wamn/ui/styles.css", replacement: `${web}ui/src/styles.css` },
        { find: /^@wamn\/ui$/, replacement: `${web}ui/src/index.ts` },
        { find: /^@wamn\/shell$/, replacement: `${web}shell/src/index.ts` },
        { find: options.client.name, replacement: at(options.client.path) },
        { find: /^solid-js$/, replacement: installed("solid-js") },
        { find: /^@solidjs\/router$/, replacement: installed("@solidjs/router") },
        { find: /^@tanstack\/solid-table$/, replacement: installed("@tanstack/solid-table") },
        { find: /^@tanstack\/solid-form$/, replacement: installed("@tanstack/solid-form") },
        { find: /^zod$/, replacement: installed("zod") },
      ],
      // web/ui installs its own libraries, and they import solid-js. Two
      // copies of solid-js break context and reactivity.
      dedupe: ["solid-js", "@solidjs/router", "@tanstack/solid-table"],
    },
    server: {
      port: options.port,
      ...(options.command === "serve" ? { proxy: proxy() } : {}),
      // The web/ui stylesheet names its font files by path, and a path outside
      // the application is refused unless it is allowed here.
      fs: { allow: [at("."), web, at(options.client.path)] },
    },
  };
}
