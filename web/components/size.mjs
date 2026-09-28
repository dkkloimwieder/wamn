/**
 * Prints the bundle size of each application web client, in gzip kB: the
 * entry, each route, and the largest chunk (wamn-4bo2).
 *
 * A route is a module that the application loads with a dynamic import. Its
 * size is every chunk it loads beyond the entry. The script builds each
 * application into a temporary directory and sets no limit.
 */

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const root = fileURLToPath(new URL("../..", import.meta.url));
const APPLICATIONS = [
  { name: "Receiving", path: "apps/wamn_receiving/web" },
  { name: "WMS", path: "apps/wamn_wms/web" },
];

const kb = (bytes) => (bytes / 1000).toFixed(2);

function measure(application) {
  const out = mkdtempSync(join(tmpdir(), "wamn-size-"));
  try {
    execFileSync("pnpm", ["exec", "vite", "build", "--manifest", "--outDir", out, "--emptyOutDir"], {
      cwd: join(root, application.path),
      // The build states an org and a project, which do not change the size.
      env: { ...process.env, WAMN_ORG: "size", WAMN_PROJECT: "size" },
      stdio: "ignore",
    });
    const manifest = JSON.parse(readFileSync(join(out, ".vite/manifest.json"), "utf8"));
    const gzip = new Map(
      Object.values(manifest).map((chunk) => [chunk.file, gzipSync(readFileSync(join(out, chunk.file))).length]),
    );
    const closure = (key, seen = new Set()) => {
      if (!seen.has(key)) {
        seen.add(key);
        for (const next of manifest[key].imports ?? []) {
          closure(next, seen);
        }
      }
      return seen;
    };
    const sum = (keys) => [...keys].reduce((total, key) => total + gzip.get(manifest[key].file), 0);
    const entryKey = Object.keys(manifest).find((key) => manifest[key].isEntry);
    const entry = closure(entryKey);
    const routes = Object.keys(manifest)
      .filter((key) => manifest[key].isDynamicEntry)
      .map((key) => ({
        name: key
          .replace(/.*\//, "")
          .replace(/\.[jt]sx?$/, "")
          .replace(/^_|-[\w-]{8}$/g, ""),
        size: sum([...closure(key)].filter((chunk) => !entry.has(chunk))),
      }))
      .sort((a, b) => a.name.localeCompare(b.name));
    const [largest] = Object.keys(manifest)
      .filter((key) => key !== entryKey)
      .map((key) => ({ file: manifest[key].file.replace(/^assets\//, ""), size: gzip.get(manifest[key].file) }))
      .sort((a, b) => b.size - a.size);
    return { entry: sum(entry), routes, largest };
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}

for (const application of APPLICATIONS) {
  const { entry, routes, largest } = measure(application);
  console.log(`${application.name}, gzip kB (each route and each lazy part: every chunk it loads beyond the entry)`);
  console.log(`  ${"entry".padEnd(22)} ${kb(entry).padStart(8)}`);
  for (const route of routes) {
    console.log(`  ${route.name.padEnd(22)} ${kb(route.size).padStart(8)}`);
  }
  console.log(`  largest chunk ${largest.file} ${kb(largest.size)}`);
}
