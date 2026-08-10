# Client workspace

The client packages use Node 22.22.1 and the Corepack-pinned pnpm recorded in
the root `package.json`. From a clean checkout, run:

```bash
node clients/scripts/check-dependency-policy.mjs
corepack enable
corepack install
pnpm --version
pnpm install --frozen-lockfile
pnpm run client:build
pnpm run client:test
```

`clients/dependency-policy.json` is the direct-dependency allowlist. Every
direct dependency must use its approved exact stable version; ranges,
prereleases, aliases, URLs, Git sources, and unapproved lifecycle scripts fail
the policy gate. pnpm's pinned 24-hour release-age policy remains active. The
frozen install verifies that manifests and the committed lockfile agree and
checks the lockfile's package integrities.

The initial reviewed stack is Solid `1.9.14`, Vite `8.2.0`,
`vite-plugin-solid` `2.11.14`, TypeScript `7.0.2`, and Vitest `4.1.10`.

The workspace reserves `clients/authoring-client` for the generated public API
client and `clients/loop-console` for the later polling console. This scaffold
contains no product screen, API adapter, hosting configuration, or platform
authority.

## The headless `wamn` CLI (`wamn-ftfc.14`)

`clients/authoring-client` also ships the reference checkout client. It speaks
HTTP through the generated client (`src/client.ts`) and adds no second transport,
no route of its own, and no frontend dependency.

```bash
node clients/authoring-client/scripts/wamn.mjs --help

# Edit a flow file, then save its exact bytes and validate the saved revision:
node clients/authoring-client/scripts/wamn.mjs validate \
  --base-url http://HOST:PORT --credential /path/to/principal.env \
  --project receiving --environment dev \
  --file flows/receive-material.flow.json \
  --draft-id draft-receiving --flow-id receive-material \
  --suite-id receiving-happy-path --flow-version 3
# then: draft-run --input FILE | suite-run | runs | promote
```

Five verbs cover the whole public contract: `validate` sends `save-flow-draft`
followed by `validate`, `promote` sends `publish`, and `runs` reads
`suite-projection`. Each invocation writes exactly one JSON document to stdout —
typed identities, a typed product refusal, a typed `unmounted` answer when the
surface has not mounted that command kind (`501`), or a fault — and exits `0`,
`3`, `4`, `5`, or `2` for a usage error. The human transcript, including
`edit-to-run-ms`, goes to stderr. Public identities the loop returns are cached
in `.wamn/state.json` (override with `--state`, disable with `--no-state`) so the
next verb needs no copied ids; no credential is ever written there.

Authentication is the first-party PAT flow: `--credential FILE` is a mode-600
`subject=`/`secret=` file exchanged at the reserved `POST /login`, or
`--token-file FILE` presents a token already issued. Nothing is read from the
environment — the CLI's only capabilities are the ones
`scripts/wamn.mjs` injects, which are POST-only HTTP, reads and writes of files
the caller named, and a read-only `git` query for its own checkout provenance.

`scripts/wamn.mjs` compiles the package with `tsc` on first use and caches the
build under the system temporary directory, so the first invocation is slower
than the rest.

Gates: `node scripts/test.mjs` (drift, typed answers, and the no-shortcut checks,
all network-free) and `node scripts/cycle.mjs` (the composed edit-to-publish
cycle against a live surface). Both are recorded in the
`[6A / wamn-ftfc.14]` section of `docs/archive/build-and-test.md`.
