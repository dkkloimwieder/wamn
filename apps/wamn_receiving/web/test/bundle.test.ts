// @vitest-environment node
// The browser loads the built chunks as modules, so each one must parse. A
// chunk that keeps source-only syntax, such as standard decorators, leaves the
// page blank (wamn-5e0e2).
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { expect, test } from "vitest";

test("every built chunk parses as a module", async () => {
  const out = mkdtempSync(join(tmpdir(), "wamn-receiving-bundle-"));
  try {
    await build({
      configFile: fileURLToPath(new URL("../vite.config.ts", import.meta.url)),
      mode: "production",
      logLevel: "silent",
      build: { outDir: out, emptyOutDir: true },
    });
    const chunks = readdirSync(join(out, "assets")).filter((name) => name.endsWith(".js"));
    expect(chunks.length).toBeGreaterThan(0);
    for (const chunk of chunks) {
      const module = join(out, `${chunk}.mjs`);
      copyFileSync(join(out, "assets", chunk), module);
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
}, 180_000);
