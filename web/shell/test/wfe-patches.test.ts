// @vitest-environment jsdom
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { makeInteractOutside } from "@solid-primitives/interaction";
import { expect, it, vi } from "vitest";

// The root install applies the dependency repairs that the pinned wfe declares.
const root = join(dirname(fileURLToPath(import.meta.url)), "../../../");
const wfe = join(root, "web/wfe/");

interface Repair {
  package: string;
  version: string;
  patch: string;
  target: string;
  localSha256: string;
}

const repairs: Repair[] = JSON.parse(readFileSync(join(wfe, "patches/provenance.json"), "utf8")).patches;

function patchDeclarations(file: string): Record<string, string> {
  const block = readFileSync(file, "utf8").match(/^patchedDependencies:\n((?:[ \t].*\n)*)/m)?.[1] ?? "";
  return Object.fromEntries(
    block
      .split("\n")
      .filter((line) => line.trim())
      .map((line) => {
        const [, name, path] = line.match(/^ {2}["']([^"']+)["']: (\S+)$/) ?? [];
        return [name, path];
      }),
  );
}

it("declares every wfe patch from the submodule", () => {
  const declared = patchDeclarations(join(wfe, "pnpm-workspace.yaml"));
  expect(Object.keys(declared)).toEqual(repairs.map((repair) => `${repair.package}@${repair.version}`));
  expect(patchDeclarations(join(root, "pnpm-workspace.yaml"))).toEqual(
    Object.fromEntries(Object.entries(declared).map(([name, path]) => [name, `web/wfe/${path}`])),
  );
});

it("installs the repaired bytes that wfe records", () => {
  const require = createRequire(join(wfe, "packages/components/package.json"));
  for (const repair of repairs) {
    // The package exports do not list package.json, so search the resolution paths.
    const directory = (require.resolve.paths(repair.package) ?? [])
      .map((path) => join(path, repair.package))
      .find((path) => existsSync(join(path, "package.json")));
    expect(directory, repair.package).toBeDefined();
    const bytes = readFileSync(join(directory!, repair.target));
    expect(createHash("sha256").update(bytes).digest("hex"), repair.package).toBe(repair.localSha256);
  }
});

it("reports outside focus before the pointer listener starts", () => {
  vi.useFakeTimers();
  const inside = document.body.appendChild(document.createElement("div"));
  const outside = document.body.appendChild(document.createElement("button"));
  const onFocusOutside = vi.fn();
  const dispose = makeInteractOutside(inside, { onFocusOutside });
  try {
    outside.focus();
    expect(onFocusOutside).toHaveBeenCalledTimes(1);
  } finally {
    dispose();
    inside.remove();
    outside.remove();
    vi.useRealTimers();
  }
});
