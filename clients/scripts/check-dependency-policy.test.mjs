import assert from "node:assert/strict";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import { checkWorkspace } from "./check-dependency-policy.mjs";

const sourceRoot = resolve(import.meta.dirname, "..", "..");

async function mutant(change) {
  const root = await mkdtemp(join(tmpdir(), "wamn-client-policy-"));
  await cp(join(sourceRoot, "package.json"), join(root, "package.json"));
  await cp(join(sourceRoot, "pnpm-workspace.yaml"), join(root, "pnpm-workspace.yaml"));
  await cp(join(sourceRoot, "pnpm-lock.yaml"), join(root, "pnpm-lock.yaml"));
  await cp(join(sourceRoot, ".github"), join(root, ".github"), { recursive: true });
  await cp(join(sourceRoot, "clients"), join(root, "clients"), { recursive: true });
  try {
    await change(root);
    await assert.rejects(checkWorkspace(root));
  } finally {
    await rm(root, { recursive: true });
  }
}

async function variant(change) {
  const root = await mkdtemp(join(tmpdir(), "wamn-client-policy-"));
  await cp(join(sourceRoot, "package.json"), join(root, "package.json"));
  await cp(join(sourceRoot, "pnpm-workspace.yaml"), join(root, "pnpm-workspace.yaml"));
  await cp(join(sourceRoot, "pnpm-lock.yaml"), join(root, "pnpm-lock.yaml"));
  await cp(join(sourceRoot, ".github"), join(root, ".github"), { recursive: true });
  await cp(join(sourceRoot, "clients"), join(root, "clients"), { recursive: true });
  try {
    await change(root);
    await checkWorkspace(root);
  } finally {
    await rm(root, { recursive: true });
  }
}

test("accepts the committed dependency policy", async () => {
  await checkWorkspace(sourceRoot);
});

test("accepts an exact internal workspace dependency", async () => {
  await variant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["@wamn/authoring-client"] = "workspace:0.1.0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a floating internal workspace dependency", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["@wamn/authoring-client"] = "workspace:*";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a registry version for an internal package", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["@wamn/authoring-client"] = "0.1.0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a workspace protocol on an external package", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies.leftpad = "workspace:1.0.0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a prerelease range", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["solid-js"] = "^2.0.0-rc.0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an unapproved prerelease version", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["solid-js"] = "2.0.0-rc.1";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a build-metadata version", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["solid-js"] = "2.0.0-rc.0+sha.deadbeef";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a drifting version range", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["solid-js"] = "^1.9.14";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an unapproved package", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies.leftpad = "1.0.0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an exotic source", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.dependencies["solid-js"] = "git+https://example.invalid/solid.git";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an unapproved lifecycle script", async () => {
  await mutant(async (root) => {
    const path = join(root, "clients", "loop-console", "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.scripts.postinstall = "node unexpected.mjs";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an omitted release-age floor", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-workspace.yaml");
    const workspace = await readFile(path, "utf8");
    await writeFile(path, workspace.replace("minimumReleaseAge: 1440\n", ""));
  });
});

test("rejects a zero release-age floor", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-workspace.yaml");
    const workspace = await readFile(path, "utf8");
    await writeFile(path, workspace.replace("minimumReleaseAge: 1440", "minimumReleaseAge: 0"));
  });
});

test("rejects a non-1440 release-age floor", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-workspace.yaml");
    const workspace = await readFile(path, "utf8");
    await writeFile(path, workspace.replace("minimumReleaseAge: 1440", "minimumReleaseAge: 60"));
  });
});

test("rejects a release-age exclusion", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-workspace.yaml");
    const workspace = await readFile(path, "utf8");
    await writeFile(path, `${workspace}minimumReleaseAgeExclude:\n  - vite@8.2.1\n`);
  });
});

test("rejects a non-strict release-age policy", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-workspace.yaml");
    const workspace = await readFile(path, "utf8");
    await writeFile(
      path,
      workspace.replace("minimumReleaseAgeStrict: true", "minimumReleaseAgeStrict: false"),
    );
  });
});

test("rejects an npmrc release-age bypass", async () => {
  await mutant(async (root) => {
    await writeFile(join(root, ".npmrc"), "minimum-release-age=0\n");
  });
});

test("rejects a script release-age bypass", async () => {
  await mutant(async (root) => {
    const path = join(root, "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.scripts.bypass = "pnpm install --config.minimumReleaseAge=0";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects a CI release-age bypass", async () => {
  await mutant(async (root) => {
    const path = join(root, ".github", "workflows", "client.yml");
    const workflow = await readFile(path, "utf8");
    await writeFile(
      path,
      workflow.replace("--frozen-lockfile", "--frozen-lockfile --minimum-release-age=0"),
    );
  });
});

test("rejects pnpm ignore-workspace install", async () => {
  await mutant(async (root) => {
    const path = join(root, "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.scripts.bypass = "pnpm --ignore-workspace install";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an npm install", async () => {
  await mutant(async (root) => {
    const path = join(root, "package.json");
    const manifest = JSON.parse(await readFile(path, "utf8"));
    manifest.scripts.bypass = "npm install";
    await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`);
  });
});

test("rejects an npmrc workspace bypass", async () => {
  await mutant(async (root) => {
    await writeFile(join(root, ".npmrc"), "ignore-workspace=true\n");
  });
});

test("rejects removal of the canonical CI install", async () => {
  await mutant(async (root) => {
    const path = join(root, ".github", "workflows", "client.yml");
    const workflow = await readFile(path, "utf8");
    await writeFile(path, workflow.replace("      - run: pnpm install --frozen-lockfile\n", ""));
  });
});

test("rejects replacement of the canonical CI install", async () => {
  await mutant(async (root) => {
    const path = join(root, ".github", "workflows", "client.yml");
    const workflow = await readFile(path, "utf8");
    await writeFile(
      path,
      workflow.replace(
        "pnpm install --frozen-lockfile",
        "pnpm --ignore-workspace install --frozen-lockfile",
      ),
    );
  });
});

test("rejects reordering the policy and Corepack CI steps", async () => {
  await mutant(async (root) => {
    const path = join(root, ".github", "workflows", "client.yml");
    const workflow = await readFile(path, "utf8");
    await writeFile(
      path,
      workflow.replace(
        "      - run: node clients/scripts/check-dependency-policy.mjs\n      - run: corepack enable",
        "      - run: corepack enable\n      - run: node clients/scripts/check-dependency-policy.mjs",
      ),
    );
  });
});

test("rejects lockfile integrity drift", async () => {
  await mutant(async (root) => {
    const path = join(root, "pnpm-lock.yaml");
    const lockfile = await readFile(path, "utf8");
    await writeFile(path, lockfile.replace(/integrity: sha512-[^}\n]+/, "integrity: missing"));
  });
});
