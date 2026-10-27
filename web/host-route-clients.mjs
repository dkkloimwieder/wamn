/**
 * Writes the TypeScript clients of the host routes, `@wamn/control-client`
 * and `@wamn/control-org-client`, into `apps/target/wamn/wamn_control`, the
 * build output root of the applications. Git does not hold them, because the build makes them from
 * the contracts in `crates/catalog/model/src/host_route`. It then copies each
 * client into the workspace packages that name it, `wamn_control` or
 * `wamn_control_org`, in `package.json` `"wamn": { "clients": [...] }`.
 *
 * Every web script that reads the clients runs this first. Cargo runs from the
 * repository root, so it builds with the root workspace and its configuration.
 */

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

execFileSync(
  "cargo",
  [
    "run",
    "--locked",
    "--offline",
    "--quiet",
    "--package",
    "wamn-schema-generator",
    "--example",
    "materialize_host_route_client",
  ],
  { cwd: fileURLToPath(new URL("..", import.meta.url)), stdio: "inherit" },
);
