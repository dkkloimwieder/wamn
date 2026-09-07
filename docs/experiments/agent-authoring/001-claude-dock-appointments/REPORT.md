# Summary

I built the dock-appointment package at `packages/dock` and its component at
`components/application/dock`. The loop passes Migrate, Introspect, Generate,
and Build, and then stops. It stops because two files pin the component
inventory of this repository, and both files sit outside the `allowed_paths`
list in `task.json`. No component named `dock` can reach the Virtualize stage
until one row is added to each file.

The task therefore did not complete. I did not reach Activate, and no release
ran, so I exercised no operation against a running release. Everything that
does not depend on those two files is finished and proved. That is the
manifest, the migration, the authored SQL, the wirings, the attachments, the
component declaration, and the component itself. The component compiles for
`wasm32-wasip2` and exports exactly the five operation tokens the scenario
names.

I did not edit the two files. The brief tells me to say so and stop when the
task cannot be completed inside the allowed paths, and the pilot measures how
many lines fall outside those paths. Editing them and reverting them produces a green loop and a false
measurement.

# Changes

Every changed file is inside `allowed_paths`. One local commit, `25fa1256`,
carries all of them. I pushed nothing.

`packages/dock` is the new package.

- `wamn.json` declares three models (`carrier`, `dock`, `appointment`), one
  CDC-excluded claim relation (`appointment_book_command`), and five public
  operations: `carrier.create`, `dock.create`, `appointment.book`,
  `appointment.check_in`, and `appointment.query`.
- `migrations/0001_initial.sql` creates the four tables in schema `receiving`.
- `command/**` and `query/**` hold the nine authored SQL statements.
- `publication/attachments.json` publishes one HTTP route per operation.
- `publication/wirings/*.json` holds one single-node wiring per operation.
- `publication/components/dock.json.in` declares the component surface.
- `generated/**` is the Generate stage output, committed unedited.

`components/application/dock` is the package-grain component.

- `src/lib.rs` binds the world and exports one interface per operation.
- `src/operation.rs` holds the envelope adapter and the five operations.
- `src/wire.rs` holds the canonical UUID, instant, and day spellings.
- `src/error.rs` translates one transport failure into the operation
  vocabulary exactly once.
- `src/generated.rs` includes the generated statement accessors.
- `wit/deps/**` holds the three WIT packages the operation tokens name.

`components/Cargo.toml` gains `application/dock` as a workspace member, and
`components/Cargo.lock` records the new member.

## How the invariants are met

DOCK-0. `carrier.create` returns `carrier_id` and `dock.create` returns
`dock_id`. Later operations address a carrier and a dock by those values.

DOCK-1. `appointment.book` locks the dock row with `SELECT ... FOR UPDATE`
before it reads the overlap and before it writes. The read that decides
`slot_unavailable` and the write that can break the rule are one serialized
section per dock, inside one transaction. Two concurrent bookings on one dock
cannot both observe a free slot.

I reached for the exclusion constraint first, and the platform refuses it. The
detail is under "Where I got stuck".

DOCK-2. `appointment_book_command` is a claim relation keyed by
`idempotency_key`. It pre-generates the appointment identity with
`DEFAULT gen_random_uuid()` under a `UNIQUE` constraint, and it stores the
canonical command bytes. A replay reads the claim, compares the bytes, and
returns the first identity. The replay path writes nothing.

DOCK-3. A key already bound to different canonical bytes refuses with
`idempotency_conflict`. The bytes are canonical, not raw. The component parses
and re-spells every UUID and every instant before it hashes them. One booking
sent in two spellings therefore binds one key to one byte string.

DOCK-4. `appointment.check_in` moves `scheduled` to `arrived` and records the
arrival time the caller supplies. One statement locks the row, moves it, and
classifies the outcome, so the read and the write cannot separate.

DOCK-5. Check-in against an unknown appointment refuses with `not_found`. The
same statement returns the outcome `not_found` when the locked row is absent.

DOCK-6. `appointment.query` lists one dock's appointments for one UTC day,
filters by status, and orders by `slot_start` with `id` as the tie-breaker.

# How I verified

## The component's own tests

```
$ cargo test --manifest-path components/Cargo.toml -p dock --all-targets --locked --offline
   Compiling dock v0.1.0 (/home/kaalin/.cache/wamn-pilot/runs/001-claude-dock-appointments/worktree/components/application/dock)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.34s
     Running unittests src/lib.rs (components/target/debug/deps/dock-6abc68eb9313df7d)

running 9 tests
test error::tests::a_named_constraint_never_becomes_a_domain_refusal ... ok
test error::tests::transport_classes_keep_their_router_meaning ... ok
test wire::tests::a_day_is_the_half_open_utc_interval_that_names_it ... ok
test wire::tests::a_day_refuses_every_spelling_but_the_calendar_one ... ok
test wire::tests::a_result_keeps_six_digits_when_the_value_has_a_fraction ... ok
test wire::tests::a_result_spells_a_whole_second_without_a_fractional_part ... ok
test wire::tests::one_instant_has_one_canonical_command_spelling_whatever_the_caller_sent ... ok
test wire::tests::a_uuid_is_accepted_only_in_its_canonical_spelling ... ok
test wire::tests::a_slot_must_end_after_it_starts ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```
$ cargo fmt --manifest-path components/Cargo.toml -p dock -- --check
(no output, exit 0)

$ cargo clippy --manifest-path components/Cargo.toml -p dock --target wasm32-wasip2 --all-targets --locked --offline
warning: `wamn-postgres-statements` (lib) generated 10 warnings
    Checking dock v0.1.0 (.../components/application/dock)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.58s
```

The ten warnings belong to `wamn-postgres-statements`, which I did not change.
The `dock` crate raises none.

## The component builds the way the loop builds it

`tools/build-components` builds guests with `--release --target wasm32-wasip2`
and one remapped path prefix. I ran the same build by hand.

```
$ RUSTFLAGS="--remap-path-prefix=$(pwd -P)=/wamn" \
    cargo build --manifest-path components/Cargo.toml -p dock \
      --target wasm32-wasip2 --release --locked --offline
    Finished `release` profile [optimized] target(s) in 8.13s

$ sha256sum components/target/wasm32-wasip2/release/dock.wasm
a9a8117fe1513c4e3617240b32fce74773f0f6b7484c8ee28d25a16bbba4246a  components/target/wasm32-wasip2/release/dock.wasm
```

The built component exports exactly the five tokens the scenario names, and it
imports exactly what the shipped `receiving` component imports.

```
$ wasm-tools component wit components/target/wasm32-wasip2/release/dock.wasm
world root {
  import wamn:postgres/types@0.1.0;
  import wamn:postgres/statements@0.1.0;
  import wamn:node/types@0.1.0;
  import wasi:io/poll@0.2.9;
  import wasi:clocks/monotonic-clock@0.2.9;
  import wasi:io/error@0.2.9;
  import wasi:io/streams@0.2.9;
  import wasi:cli/stdout@0.2.9;
  import wasi:cli/stderr@0.2.9;
  import wasi:cli/stdin@0.2.9;
  import wasi:cli/environment@0.2.9;
  import wasi:cli/exit@0.2.9;
  import wasi:cli/terminal-input@0.2.9;
  import wasi:cli/terminal-output@0.2.9;
  import wasi:cli/terminal-stdin@0.2.9;
  import wasi:cli/terminal-stdout@0.2.9;
  import wasi:cli/terminal-stderr@0.2.9;

  export dock:carrier/create@0.1.0;
  export dock:dock/create@0.1.0;
  export dock:appointment/book@0.1.0;
  export dock:appointment/check-in@0.1.0;
  export dock:appointment/query@0.1.0;
}
```

The same command against the shipped `receiving.wasm` prints the identical
import list. That is the evidence that Virtualize and Admit treat my component
the way they treat an existing product component.

## The loop

Run 002 ran with `packages/dock` complete and `components/application/dock`
absent from the workspace. Migrate, Introspect, Generate, and Build all
passed. Virtualize refused.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied dock@0.1.0: 1 migration(s)
Error: dev-stage-failed at virtualize: dev-stage-state-invalid while select package component artifact: dock@0.1.0 component dock derived build package dock with 0 artifact matches

Caused by:
    dev-stage-state-invalid while select package component artifact: dock@0.1.0 component dock derived build package dock with 0 artifact matches
```

Run 003 and run 004 ran with `application/dock` added to
`components/Cargo.toml`. Run 004 ran from the clean committed tree. Build
refused both times.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied dock@0.1.0: 1 migration(s)
Error: dev-stage-failed at build: dev-stage-state-invalid while build production components: command exited with exit status: 65: build-components: component profile, canonical inventory, and locked metadata drifted

Caused by:
    dev-stage-state-invalid while build production components: command exited with exit status: 65: build-components: component profile, canonical inventory, and locked metadata drifted
exit=1
```

Three attempts, one failure, one cause. I stopped there.

Run 002 proves the package content itself. Migrate applied the migration to a
fresh verification database. Introspect read the catalog back. Generate wrote
58 files under `packages/dock/generated/`. They include `package-weld.json`,
the five operation contracts, the native verifier siblings, and the statement
accessors that carry the admitted digests. The stage sequence is the proof
that the manifest, the migration, and the nine authored SQL statements all
satisfy the generator, the migration validator, and the catalog introspector.

## What I did not verify, and why

I exercised none of the five operations against a running release. There is no
running release. The loop stops at Build, six stages before Activate, so
Publish, Apply, Acl, Release, and Activate never ran, and no route was ever
served. `wamn dev --hold` cannot help, because it holds a release the loop
never mints.

The following are therefore unproved by execution, and only the code and the
SQL stand behind them.

- The claim replay returns the first appointment id (DOCK-2).
- A rebound key refuses with `idempotency_conflict` (DOCK-3).
- Two concurrent bookings on one slot produce exactly one `slot_unavailable`
  (DOCK-1).
- Check-in returns `arrived` and the supplied arrival time (DOCK-4).
- Check-in against an unknown id refuses with `not_found` (DOCK-5).
- The day list comes back in slot order (DOCK-6).

I also did not exercise the SQL against a live database by hand. The brief
grants the PostgreSQL URLs for inspection only, and the verification database
is disposable: the loop drops it when a stage fails, so it was already gone.

The nine unit tests cover the pure decisions the operations make. Those are
the canonical UUID spelling, the canonical instant spelling, the result
instant spelling, the day interval, the slot ordering rule, and the
transport-to-operation error mapping. They do not cover the transaction bodies, which need
a database.

# Decisions

Nobody was available to answer, so I decided and recorded.

Package identity is `dock` at version `0.1.0`. The design authority says every
package, WIT, wire, and schema version stays at `0.1` through MVP, so I used
`0.1.0` rather than copying the `1.0.0` of the base Receiving package. The
package id matches the overlay root directory the task names.

The package declares no base dependency. The scenario touches nothing the
Receiving package owns, so `packages/receiving` stays an ignored package
source and the closure holds one package. This also keeps the catalog
projection clean, because every table in the verification database has an
owner in my manifest.

The tables live in schema `receiving`. The guest never qualifies a table name,
and the host selects one schema per workload from the deployment
configuration, which names `receiving`. A table in a schema of its own is
unreachable from the guest SQL.

`carrier.create` and `dock.create` take no idempotency key. The exit gate
sends a body with a name and nothing else, and the scenario asks only that
each returns an identity. The claim law applies where the scenario asks for
it, which is `appointment.book`.

`appointment.query` is a projection with result class `one`, not a generated
CRUD query. The scenario filters by a calendar day, and a generated query
binds filters as JSON arrays of exact values, which cannot express a day
range. The result is one object carrying `dock_id`, `day`, and an
`appointments` array.

A day is a UTC day. The scenario names a day and no zone, and the slots it
books carry `Z`. A zoned day is a deployment binding this package does not
have.

A result spells an instant as RFC 3339 in UTC with a `Z`. It uses the fewest
digits that still name the stored value. A whole second carries no fractional
part, and every other value carries six digits. The naming law fixes six fractional digits for
cursor keys and for durable command bytes, and I follow it there. It does not
govern a result payload, and the exit gate pins `arrived_at` to
`2026-10-01T09:07:00Z`. The SQL `ORDER BY` stays the ordering authority,
because two spellings of different precision do not compare lexically.

An input instant is accepted in any RFC 3339 spelling and re-spelled before
use. The base Receiving package instead refuses any input that is not already
six-digit UTC. I diverged because the exit gate sends `2026-10-01T09:00:00Z`,
and because the manifest's own canonicalization rule says to parse and
re-spell rather than to check a shape.

An input UUID is accepted only in its canonical lowercase hyphenated
spelling. Here I followed the base package rather than diverging, because the
gate sends canonical UUIDs and the stricter rule costs nothing.

`appointment.check_in` names one refusal of its own,
`invalid_status_transition`, for a check-in against an appointment that is no
longer scheduled. Status only moves forward, and a silent second success hides
that.

The status vocabulary carries `departed`, and no operation reaches it. The
scenario describes the transition and pins no operation for it, so I recorded
the value and stopped short of inventing a wire name the exit gate does not
name.

No named constraint reaches a caller. Every refusal a caller can observe is
decided before the write, so a constraint that still fires is a defect and
becomes `internal_error`.

# Where I got stuck

## The blocker: two files outside the allowed paths pin the component inventory

`wamn dev` builds guests through `tools/build-components`, and that script
does not discover components. It reads two hand-maintained inventories and
refuses any drift between them and the live Cargo metadata.

`architecture/workspace-tiers.json` pins three facts that a new component
breaks.

1. `source_inventory.component_workspaces[0].package_count` is `20`. Adding
   `application/dock` makes the live member count `21`.
2. `tiers.full_ci.component_packages` must equal the union of the members of
   both component workspaces. Adding `dock` makes the two sets differ by
   exactly `dock`.
3. `tiers.product_components.component_packages` selects what the `m1` profile
   builds, and `profiles.components.expected_package_counts.m1` pins that list
   at `9`. A tenth entry needs both rows.

`tools/component-virtualization.json` pins the fourth fact. Its `artifacts`
list is an allowlist of four packages, and the Virtualize stage selects a
package component only when that list names it. The comment in
`tools/build-components` states the intent plainly: "This is an allowlist, not
discovery."

`task.json` `allowed_paths` is `packages/dock/**`,
`components/application/dock/**`, `components/Cargo.toml`, and
`components/Cargo.lock`. Neither inventory file is in it.

The two refusals are the two halves of one cause.

- Leave `application/dock` out of `components/Cargo.toml`, and the build
  succeeds while the Virtualize stage finds no artifact for the package
  component: `dock@0.1.0 component dock derived build package dock with 0
  artifact matches`.
- Add it, and the build itself refuses: `component profile, canonical
  inventory, and locked metadata drifted`.

Here is the exact drift, measured against the committed tree.

```
$ cargo metadata --manifest-path components/Cargo.toml --locked --offline --no-deps --format-version 1
components workspace members: 21 declared package_count: 20
full_ci list == members union: False
missing from full_ci: ['dock']
product_components: ['blob-put', 'client-acme-receiving', 'http-request', 'http-route', 'label-render', 'materializer', 'receiving', 'transform', 'wms']
virtualization artifacts: ['blob-put', 'client-acme-receiving', 'receiving', 'wms']
```

Four rows clear it, and all four are outside my allowed paths.

- `architecture/workspace-tiers.json`:
  `source_inventory.component_workspaces[0].package_count` from `20` to `21`.
- `architecture/workspace-tiers.json`: add `dock` to
  `tiers.full_ci.component_packages`.
- `architecture/workspace-tiers.json`: add `dock` to
  `tiers.product_components.component_packages`, and raise
  `profiles.components.expected_package_counts.m1` from `9` to `10`.
- `tools/component-virtualization.json`: add one artifact entry with package
  `dock`, workspace manifest `components/Cargo.toml`, raw file `dock.wasm`,
  and output file `dock.wasm`.

Stall category: `provisioning`. Stage: Build, and Virtualize when the member
is absent. The message names inventory drift and never names the file that
must change, so the pointer costs a reader a walk through
`tools/build-components` and its jq guards.

## The exclusion constraint is refused, and the protocol document says otherwise

The experiment protocol at `docs/experiments/agent-authoring/protocol.md`
records a probe of `EXCLUDE USING gist` for exactly this invariant. It
concludes that the constraint is admitted inside `CREATE TABLE` and that
`CREATE EXTENSION btree_gist` "passes the statement allowlist, because
`extension` is one of the admitted object kinds". At this commit the code says
the opposite on both counts, and the exclusion path fails at a third place the
probe did not reach.

1. `CREATE EXTENSION` is refused. In
   `crates/schema/introspection/src/migration_policy.rs`, `is_ruled_operation`
   lists `"extension"` at line 787 among the refused object classes, and
   `validate_statement` turns that into "the statement operates on a refused
   object class" at line 453. `btree_gist` is therefore unreachable from a
   migration, and without it a `gist` exclusion over `dock_id WITH =` cannot be
   created at all.
2. Catalog introspection refuses the constraint even when it exists.
   `map_constraints` in `crates/schema/introspection/src/postgres.rs` matches
   `contype` values `p`, `u`, `f`, and `c`, skips `n`, and refuses everything
   else at line 1674 with "unsupported pg_constraint contype `x`".
3. Catalog introspection also refuses the backing index. `map_indexes` refuses
   any index that is an exclusion index, is not `btree`, or carries an
   expression, at line 1729.

The Introspect stage runs immediately after Migrate, so an exclusion
constraint that the migration validator lets through stops the loop one stage
later.

The same three rules block the smaller fallback. There is no `tstzrange`
column, because `postgres_type` in `crates/schema/introspection/src/ir.rs`
admits ten types and no range type. There is no secondary index at all,
because `validate_statement` admits only `CREATE TABLE` and `ALTER TABLE`.

So the lock-and-check form is not a second choice on this platform. It is the
only database form available, and the protocol's own rubric scores the two
forms the same.

Stall category: `rule-unknown`. It points at the protocol document rather than
at the product. I lost time reading a probe result that no longer matches the
code.

## A smaller one: an empty CARGO_TARGET_DIR

The shell this run starts with exports `CARGO_TARGET_DIR` as an empty string.
Every Cargo command refuses with "the target directory is set to an empty
string in the `CARGO_TARGET_DIR` environment variable", and
`tools/build-components` reports it as "locked component Cargo metadata failed
for components/Cargo.toml". I ran every Cargo command and every `wamn dev`
under `env -u CARGO_TARGET_DIR`. Stall category: `env`.

# Rules I relied on

`docs/architecture/application-naming.md` gave me the operation token form
`<package-id-kebab>:<module-kebab>/<action-kebab>@<package-version>`. It also
gave me the rule that a permission token equals its package-local operation
identity, and the constraint naming convention
`<table>_<column_1>[_<column_n>]_<kind>` in table definition order. It says
that migrations qualify their DDL and that the SQL corpus never does.

`docs/exe-model.md` gave me three rules. Every package, WIT, wire, and schema
version stays at `0.1` through MVP. State is rows and operations, with nothing
invented outside the database. The authorization path is never cached, which
is why my package writes no permission of its own and lets `apply-package`
derive the grants from the manifest.

`crates/schema/generator/src/manifest.rs` is the closed vocabulary the
manifest must match. It fixes the shared error-detail keys each error code
declares, and the rule that a command with local SQL declares `transaction`
and `automatic_retry` together.

The claim law is stated in `crates/schema/generator/src/manifest.rs` on
`ClaimDeclaration`. Any identity a command creates comes from the claim, not
from the work. A create that mints its row id during the work mints a second
id on replay. The same file's `CommandCanonicalization` states
"canonicalize, then hash", and warns that an uppercase UUID validates
perfectly and then hashes to different bytes. Both rules shaped
`appointment.book`.

`crates/schema/introspection/src/migration_policy.rs` and
`crates/schema/introspection/src/postgres.rs` are the real limits on a
migration: two admitted statement kinds, ten column types, eight column
defaults, four constraint kinds, and btree indexes over named columns only.

`packages/receiving` and `components/data/receiving-data` are the working
reference. They gave me the envelope shape, the per-item result and refusal
shape, and the lock-and-classify SQL pattern. They also gave me the rule that
no named constraint reaches a caller unless the operation contract names it.

`CLAUDE.md` told me to keep errors as contextual structs with a kind, a
source, and context, and to translate them exactly once at the owning
boundary. `src/error.rs` and `src/operation.rs` follow that split.

The brief told me to commit locally and never push, and to say so and stop if
the task cannot be completed inside the allowed paths. I did both.

# Open questions

Is the component inventory meant to be part of an authoring surface? Today a
new package cannot ship a new component without editing
`architecture/workspace-tiers.json` and `tools/component-virtualization.json`,
and both files carry conformance tests. If package authoring is a first-class
activity, then a new component under `components/application/` is the ordinary
case, and the inventory is a derived fact rather than an authored one.

Does the `allowed_paths` list for this task need those two files? The pilot
tooling specification at `docs/poc/agent-authoring-tooling-spec.md` line 300
shows the same three-entry pattern this task uses, so the gap looks general
rather than specific to the dock task.

What is the wire spelling of a timestamp in a result? The naming law fixes six
fractional digits for cursor keys and durable command bytes and says nothing
about results. The base Receiving package passes the PostgreSQL text spelling
straight through. I normalize instead. One rule stated in one place settles
it.

Does `appointment.book` also need an exclusion constraint once the platform
admits one? The lock makes the refusal typed and the invariant true. A
constraint makes the invariant true against a writer that never takes the
lock. Today the guest role has no other writer, so the question is about the
future, not about this release.

Does the protocol's `EXCLUDE` probe need re-running? Its recorded answer
disagrees with the code at this commit on `CREATE EXTENSION`. It also does not
mention the Introspect stage. That stage refuses the constraint and its index
independently of the migration validator.
