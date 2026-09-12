# Explicit exclusion ownership

Issue: `wamn-10yt.79`.
Base: `df0ce242014b48b3454fce2a742d58056d750e15`.

The generator rejected an explicit owner for an exclusion that PostgreSQL introspection already knew.
Its model validator searched ordinary constraints but omitted the separate exclusion list.
The validator now searches both lists before it refuses an unknown constraint.
The existing owner rule still permits only the package or one of its declared bases.
No ownership rule, error vocabulary, manifest, or generated package output changes.

Three regression tests exercise the public generator.
The acceptance test generates an exact, closed exclusion refusal for both an explicit package owner and a declared base owner.
The refusal tests retain the exact errors for an absent constraint and an undeclared owner.
The full generation target passes 58 tests with no ignored or filtered tests in `unit-001`.
That first command takes 50.264 seconds, including the cache rebuild.

The negative control removes only the four-line exclusion lookup from the production validator.
`mutant-001` fails the named acceptance test after successful compilation.
The failure reports the original unknown-constraint error for the actual exclusion.
The restored source has the same SHA-256 digest, and `unit-002` passes all 58 tests again in 4.431 seconds.
The mutated source is not part of this change.

Scoped Clippy passes under the repository lint policy in `unit-003` after 40.228 seconds.
It reports seven catalog warnings, five generator library warnings, and three generation test warnings.
Those warnings point outside the new test cases and production lookup.
This run does not deny every existing warning or claim a separate baseline lint comparison.

The first PostgreSQL attempt stopped at its readiness command after 2.025 seconds.
`pg_isready` returned exit 2 and reported no response before the tool created the application schema.
The tool removed its disposable database container and retained the unchanged source hashes.
No real-database ownership assertion ran in this attempt.
The owner approved a 30-second startup limit before a fresh proof attempt.
The tool now waits for TCP readiness and retains container logs if an attempt fails.

`postgres-002` passes in 3.299 seconds after five readiness attempts.
The normal materializer introspects real exclusions in a fresh PostgreSQL 18 database.
Receiving names its own exclusion owner, and Acme names both the base owner and its own exclusion owner.
Each package generates the exact reachable exclusion refusal.
Both packages refuse an absent constraint name and an undeclared owner before they change any generated artifact.
The tool removes its temporary package copies and disposable database, and the Rust source hashes remain unchanged.

Run the focused generator suite from an isolated worktree:

```bash
cargo test --locked --offline -p wamn-schema-generator --test generation -- --include-ignored
```

Run the PostgreSQL proof with a new evidence directory:

```bash
python3 docs/perf/2026.09/exclusion-owner/tools/postgres.py \
  --tree "$PWD" --evidence-dir /path/to/new/exclusion-owner-proof
```

Focused Rust formatting, Python syntax parsing, and `git diff --check` pass.

These are local generator proofs.
No full workspace sweep, deployed host invocation, or cluster gate ran for this change.
