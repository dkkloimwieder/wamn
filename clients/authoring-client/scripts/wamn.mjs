#!/usr/bin/env node
// The runnable `wamn` CLI (wamn-ftfc.14).
//
//   node clients/authoring-client/scripts/wamn.mjs --help
//   node clients/authoring-client/scripts/wamn.mjs test-set-run \
//     --base-url http://HOST:PORT --token-file /path/to/pat \
//     --project receiving --environment dev \
//     --validated-draft sha256:...
//
// This file is the ONLY place the CLI touches the platform-neutral outside
// world. Everything it hands `runCli` is listed here: POST-only HTTP, reads and
// writes of files the caller named, a clock, and two output streams. There is
// deliberately no environment reader, NO PROCESS SPAWN AT ALL, and no database
// client — the CLI cannot acquire an endpoint, a credential, or storage
// authority it was not handed on the command line.
//
// The `git` reader left with `save-draft` (wamn-0h0g.8.5.5): provenance had
// exactly one wire carrier, so with that command gone nothing can consume a
// commit claim and the capability is withdrawn rather than left dangling.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";

import { compiledPackage } from "./compile.mjs";

const io = {
  now: () => Date.now(),
  fetch: (endpoint, init) =>
    fetch(endpoint, { body: init.body, headers: init.headers, method: init.method }),
  readText: (path) => readFile(path, "utf8"),
  readJson: async (path) => {
    try {
      return JSON.parse(await readFile(path, "utf8"));
    } catch (error) {
      if (error.code === "ENOENT") return undefined;
      throw error;
    }
  },
  writeJson: async (path, value) => {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
  },
  out: (line) => process.stdout.write(`${line}\n`),
  err: (line) => process.stderr.write(`${line}\n`),
};

const { runCli } = await import(join(await compiledPackage(), "cli", "cli.js"));
process.exitCode = await runCli(process.argv.slice(2), io);
