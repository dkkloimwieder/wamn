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
