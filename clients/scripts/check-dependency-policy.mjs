import { readFile, readdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const dependencyKinds = [
  "dependencies",
  "devDependencies",
  "optionalDependencies",
  "peerDependencies",
];

const lifecycleScripts = new Set([
  "dependencies",
  "install",
  "postinstall",
  "postpack",
  "postpublish",
  "preinstall",
  "prepack",
  "prepare",
  "prepublish",
  "prepublishOnly",
  "publish",
]);

const exactStableVersion = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/;
const releaseAgeSetting = /minimum[-_.]?release[-_.]?age/i;
const workspaceBypassSetting = /ignore[-_.]?workspace/i;
const packageManagers = new Set(["bun", "npm", "npx", "pnpm", "yarn"]);
const installActions = new Set(["ci", "i", "install"]);
const canonicalClientRunSteps = [
  "node clients/scripts/check-dependency-policy.mjs",
  "corepack enable",
  "corepack install",
  "pnpm --version",
  "pnpm install --frozen-lockfile",
  "pnpm run client:build",
  "pnpm run client:test",
];

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function readOptional(path) {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

function requireCondition(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function packageManagerInvocations(command) {
  const invocations = [];
  for (const segment of command.split(/&&|\|\||;|\n/)) {
    const tokens = segment.trim().split(/\s+/).filter(Boolean);
    const managerIndex = tokens.findIndex((token) => packageManagers.has(token));
    if (managerIndex === -1) continue;

    const manager = tokens[managerIndex];
    const arguments_ = tokens.slice(managerIndex + 1);
    const action = arguments_.find(
      (argument) => installActions.has(argument) || argument === "config",
    );
    if (action !== undefined) invocations.push({ action, manager });
  }
  return invocations;
}

function commandUsesUnapprovedInstallerOrConfig(command) {
  return packageManagerInvocations(command).some(({ action, manager }) => {
    if (action === "config") return true;
    if (manager !== "pnpm") return true;
    return workspaceBypassSetting.test(command);
  });
}

async function packageManifestPaths(root) {
  const clients = join(root, "clients");
  const entries = await readdir(clients, { withFileTypes: true });
  const paths = [join(root, "package.json")];

  for (const entry of entries) {
    if (!entry.isDirectory() || entry.name === "scripts") continue;
    const path = join(clients, entry.name, "package.json");
    try {
      await readFile(path);
      paths.push(path);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }

  return paths;
}

export async function checkWorkspace(root) {
  const policy = await readJson(join(root, "clients", "dependency-policy.json"));
  const rootManifest = await readJson(join(root, "package.json"));
  requireCondition(
    rootManifest.packageManager === policy.packageManager,
    "root packageManager does not match the approved Corepack identity",
  );

  const allowed = policy.allowedDirectDependencies ?? {};
  const allowedLifecycle = new Set(policy.allowedLifecycleScripts ?? []);

  for (const path of await packageManifestPaths(root)) {
    const manifest = await readJson(path);
    const relativePath = path.slice(root.length + 1);
    requireCondition(
      !releaseAgeSetting.test(JSON.stringify(manifest.pnpm ?? {})),
      `${relativePath}: release-age policy belongs only in pnpm-workspace.yaml`,
    );

    const npmrc = await readOptional(join(dirname(path), ".npmrc"));
    requireCondition(
      npmrc === undefined ||
        (!releaseAgeSetting.test(npmrc) && !workspaceBypassSetting.test(npmrc)),
      `${relativePath}: .npmrc must not override workspace supply-chain policy`,
    );

    for (const [name, command] of Object.entries(manifest.scripts ?? {})) {
      requireCondition(
        !releaseAgeSetting.test(command),
        `${relativePath}: script ${name} must not override release-age policy`,
      );
      requireCondition(
        !commandUsesUnapprovedInstallerOrConfig(command),
        `${relativePath}: script ${name} uses an unapproved install or config invocation`,
      );
      if (lifecycleScripts.has(name) && !allowedLifecycle.has(name)) {
        throw new Error(`${relativePath}: unapproved lifecycle script ${name}: ${command}`);
      }
    }

    for (const kind of dependencyKinds) {
      for (const [name, version] of Object.entries(manifest[kind] ?? {})) {
        requireCondition(
          typeof version === "string" && exactStableVersion.test(version),
          `${relativePath}: ${name} must use an exact stable version, found ${version}`,
        );
        requireCondition(
          Object.hasOwn(allowed, name),
          `${relativePath}: ${name} is not in the direct-dependency allowlist`,
        );
        requireCondition(
          allowed[name] === version,
          `${relativePath}: ${name} must use approved version ${allowed[name]}, found ${version}`,
        );
      }
    }
  }

  const workspace = await readFile(join(root, "pnpm-workspace.yaml"), "utf8");
  requireCondition(
    /^packages:\n  - clients\/\*\n/m.test(workspace),
    "pnpm workspace must contain the clients/* package boundary",
  );
  requireCondition(
    /^onlyBuiltDependencies: \[\]\n?$/m.test(workspace),
    "dependency lifecycle scripts must remain denied by default",
  );
  requireCondition(
    /^strictDepBuilds: true$/m.test(workspace),
    "ignored dependency lifecycle scripts must fail the install",
  );
  const releaseAgeValues = [...workspace.matchAll(/^minimumReleaseAge:\s*(\S+)\s*$/gm)];
  requireCondition(
    releaseAgeValues.length === 1 && releaseAgeValues[0][1] === "1440",
    "pnpm minimumReleaseAge must be set exactly once to 1440 minutes",
  );
  const releaseAgeStrictValues = [
    ...workspace.matchAll(/^minimumReleaseAgeStrict:\s*(\S+)\s*$/gm),
  ];
  requireCondition(
    releaseAgeStrictValues.length === 1 && releaseAgeStrictValues[0][1] === "true",
    "pnpm minimumReleaseAgeStrict must remain enabled",
  );
  const releaseAgeLines = workspace
    .split("\n")
    .filter((line) => releaseAgeSetting.test(line));
  requireCondition(
    releaseAgeLines.length === 2 &&
      releaseAgeLines.includes("minimumReleaseAge: 1440") &&
      releaseAgeLines.includes("minimumReleaseAgeStrict: true"),
    "pnpm release-age policy must not use exclusions or alternate bypass forms",
  );

  const clientWorkflow = await readFile(join(root, ".github", "workflows", "client.yml"), "utf8");
  requireCondition(
    !releaseAgeSetting.test(clientWorkflow) && !workspaceBypassSetting.test(clientWorkflow),
    "client CI must not override workspace supply-chain policy",
  );
  const clientRunSteps = [...clientWorkflow.matchAll(/^\s*- run:\s*(.+?)\s*$/gm)].map(
    ([, command]) => command,
  );
  requireCondition(
    JSON.stringify(clientRunSteps) === JSON.stringify(canonicalClientRunSteps) &&
      !clientRunSteps.some(commandUsesUnapprovedInstallerOrConfig),
    "client CI supply-chain, Corepack, install, build, and test steps must remain canonical",
  );

  const lockfile = await readFile(join(root, "pnpm-lock.yaml"), "utf8");
  requireCondition(
    /^lockfileVersion: ['"]9\.0['"]$/m.test(lockfile),
    "pnpm lockfile version is missing or unapproved",
  );
  requireCondition(
    !/^\s+(?:tarball|repo|path):/m.test(lockfile),
    "pnpm lockfile contains an exotic package source",
  );

  const packageBlocks = lockfile.split(/^packages:\n/m)[1]?.split(/^snapshots:\n/m)[0];
  requireCondition(packageBlocks !== undefined, "pnpm lockfile has no packages section");
  const entries = packageBlocks.split(/^  (?=\S)/m).slice(1);
  for (const entry of entries) {
    const [packageKey] = entry.split("\n", 1);
    requireCondition(
      /^    resolution: \{integrity: sha512-[A-Za-z0-9+/]+={0,2}\}$/m.test(entry),
      `pnpm lockfile package lacks sha512 integrity: ${packageKey.replace(/:$/, "")}`,
    );
  }
}

const invokedPath = process.argv[1] && resolve(process.argv[1]);
if (invokedPath === fileURLToPath(import.meta.url)) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
  try {
    await checkWorkspace(root);
    console.log("client dependency policy: ok");
  } catch (error) {
    console.error(`client dependency policy: ${error.message}`);
    process.exitCode = 1;
  }
}
