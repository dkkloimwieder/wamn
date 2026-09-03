# Component artifact boundary — package-grain components

Status: RULED 2026-09-01 · supersedes the one-operation-per-artifact rule
(`wamn-10yt.3.3`) and the interim kind-grouping · closes the root cause of
wamn-10yt.8 (155 MiB per host).

## Definitions

- **Package** — unit of ownership and versioning: schema contribution
  (`migrations/`), operations (`custom_operations` + generated CRUD),
  authored SQL, generated contracts. `package_id@version`, immutable.
  Not configuration.
- **Component** — compiled behavior unit: one digest, one import set
  (effect posture + connection requirements), one export set. Carries no
  schema or config.
- **Palette component** — schema-less library component authored for
  low-code wiring. Independent of packages; unchanged by this spec.

## Rule

1. **Default: one component per package.** Its export set is the
   package's operation set. A package's operations version together, so
   split digests move in lockstep — splitting buys no identity, only
   duplicated runtime.
2. **Split only when import/requirement sets differ.** An operation
   needing a connection requirement or effect the rest of the package
   lacks (e.g. `integration.sync_receipt` needing `wamn:connection/http`)
   goes in its own component so wirings bind only what each node needs
   and effect posture stays honest per artifact. Second valid reason: a
   heavy dependency that must not ride in the hot pool.
3. **Grouping is declared, not inferred.** Each operation entry may name
   `component: "<name>"`; default is the package component. Names are
   package-local, snake_case. Validation refuses an empty component and
   refuses two components with identical import/requirement sets
   (a split with no reason).
4. **Never split** by taxonomy (CRUD/command/BFF), by team habit, or for
   versioning.

## Runtime consequences

- Component fact carries an **operation set** (exports), not a singular
  operation. Dispatch selects the export by operation token.
- Permission checks, static SQL, and effect facts are unchanged in
  meaning: permission is per token, SQL is per operation, effects are
  per artifact (as they were).
- Pools key by digest; one package component = one warm pool serving all
  its operations.
- Release membership records `(package, version, component digests)`.

## Migration of current tree

- Receiving base: six artifacts → one `receiving` component.
- Overlay: one `client_acme_receiving` component; `integration.*` gets
  its own component when `sync_receipt` is implemented (not now — it is
  deferred).
- wamn-10yt.8 re-measures after regeneration; expected ~1/6 size.

## Acceptance

Generator emits one component per declared group; catalog fact and
dispatch handle export sets; all eight Receiving routes green against the
single component; a manifest naming a component with an identical
requirement set refuses with a typed literal; host artifact bytes
measured and recorded on 10yt.8.
