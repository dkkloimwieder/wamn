/**
 * The browser loads the built chunks as modules, so each one must parse. A
 * chunk that keeps source-only syntax, such as standard decorators, leaves the
 * page blank (wamn-attn6, wamn-5e0e2).
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";

/**
 * Builds the application of `configFile` into a temporary directory and
 * makes sure that Node parses every emitted script as a module, the worker
 * at the root of the output included.
 */
export async function assertChunksParse(configFile: string | URL): Promise<void> {
  const out = mkdtempSync(join(tmpdir(), "wamn-bundle-"));
  try {
    await build({
      configFile: typeof configFile === "string" ? configFile : fileURLToPath(configFile),
      mode: "production",
      logLevel: "silent",
      build: { outDir: out, emptyOutDir: true },
    });
    const chunks = (readdirSync(out, { recursive: true }) as string[]).filter((name) => name.endsWith(".js"));
    if (chunks.length === 0) throw new Error("the build emitted no script");
    for (const chunk of chunks) {
      const module = join(out, `${chunk.replaceAll("/", "_")}.mjs`);
      copyFileSync(join(out, chunk), module);
      try {
        execFileSync(process.execPath, ["--check", module], { stdio: "pipe" });
      } catch (error) {
        const lines = String((error as { stderr?: unknown }).stderr ?? error).split("\n");
        const reason = lines.filter((line) => /^\w*Error:/.test(line)).join(" ");
        throw new Error(`${chunk} does not parse: ${reason}`, { cause: error });
      }
    }
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}
