// Compile this package's TypeScript to runnable JavaScript.
//
// The published surface of `@wamn/authoring-client` is TypeScript source, and
// Node in this environment has no type stripping, so anything that RUNS the CLI
// compiles it first. The build is a cache keyed by source modification times:
// the first invocation pays for `tsc`, later ones do not — which matters because
// the CLI measures edit-to-run latency and a compile inside that window would
// inflate it.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const PACKAGE_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const SOURCE_ROOT = join(PACKAGE_ROOT, "src");
const STAMP = "build-stamp.json";

/** Every TypeScript source under `src`, with its modification time. */
async function sourceStamp(directory = SOURCE_ROOT) {
  const stamp = {};
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) Object.assign(stamp, await sourceStamp(path));
    else if (entry.name.endsWith(".ts")) stamp[path] = (await stat(path)).mtimeMs;
  }
  return stamp;
}

/** The `tsc` this workspace pins when it is installed, else the one on PATH. */
function typescriptCompiler() {
  for (const candidate of [
    join(PACKAGE_ROOT, "node_modules", ".bin", "tsc"),
    join(dirname(dirname(PACKAGE_ROOT)), "node_modules", ".bin", "tsc"),
  ]) {
    if (existsSync(candidate)) return candidate;
  }
  return "tsc";
}

/** Emit JavaScript into `outDir`, failing loudly on a type error. */
export function compileTo(outDir) {
  const compile = spawnSync(
    typescriptCompiler(),
    ["--project", "tsconfig.json", "--noEmit", "false", "--outDir", outDir],
    { cwd: PACKAGE_ROOT, stdio: "inherit" },
  );
  if (compile.status !== 0) {
    throw new Error(`tsc failed with status ${compile.status ?? "signal"}`);
  }
  return outDir;
}

/**
 * Return a directory holding the compiled package, compiling only when the
 * cached build is missing or older than a source file.
 */
export async function compiledPackage() {
  const outDir = join(tmpdir(), "wamn-authoring-client-build");
  const stamp = await sourceStamp();
  let cached;
  try {
    cached = JSON.parse(await readFile(join(outDir, STAMP), "utf8"));
  } catch {
    cached = undefined;
  }
  const fresh =
    cached !== undefined &&
    JSON.stringify(cached) === JSON.stringify(stamp) &&
    existsSync(join(outDir, "cli", "cli.js"));
  if (!fresh) {
    await mkdir(outDir, { recursive: true });
    compileTo(outDir);
    await writeFile(join(outDir, STAMP), `${JSON.stringify(stamp)}\n`);
  }
  return outDir;
}
