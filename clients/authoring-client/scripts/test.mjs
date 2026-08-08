import { spawnSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const output = await mkdtemp(join(tmpdir(), "wamn-authoring-client-"));
let status = 0;

try {
  const drift = spawnSync(process.execPath, ["scripts/generate.mjs", "--check"], {
    stdio: "inherit",
  });
  if (drift.status !== 0) status = drift.status ?? 1;

  // The smoke gate's static half: the request it would send must still be the
  // checked-in collection's request. Network-free, so it belongs in this harness.
  const collectionDrift = spawnSync(process.execPath, ["scripts/smoke.mjs", "--check"], {
    stdio: "inherit",
  });
  if (collectionDrift.status !== 0) status = collectionDrift.status ?? 1;

  await writeFile(join(output, "package.json"), '{"type":"module"}\n');
  if (status === 0) {
    const compile = spawnSync(
      "tsc",
      ["--project", "tsconfig.json", "--noEmit", "false", "--outDir", output],
      { stdio: "inherit" },
    );
    if (compile.status !== 0) status = compile.status ?? 1;
  }

  if (status === 0) {
    const run = spawnSync("node", ["tests/client.test.mjs"], {
      env: {
        ...process.env,
        WAMN_AUTHORING_CLIENT_TEST_MODULE: pathToFileURL(join(output, "index.js")).href,
      },
      stdio: "inherit",
    });
    if (run.status !== 0) status = run.status ?? 1;
  }
} finally {
  await rm(output, { force: true, recursive: true });
}

process.exitCode = status;
