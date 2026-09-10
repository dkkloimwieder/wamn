# Generate privilege diagnostics

Issue: `wamn-10yt.39`.
Base: `b214506aa6cda434202e22d22941ff19390c37c8`.

The privilege refusal now shows the verified SQL and declared values for each relation.
Both objects contain sorted `select_fields`, `insert_fields`, `update_fields`, and `lock` values.
The message explains that `RETURNING` columns require reads and row-lock clauses require `lock=true`.
The comparison, refusal class, SQL parser, and accepted declarations remain unchanged.

The generation suite passed 55 tests, with zero ignored or filtered tests.
The two added cases use the shipped Receiving SQL and the public generation path.
They cover omitted `RETURNING` reads, incorrect write declarations, and an omitted `FOR UPDATE` lock.
Each assertion compares the complete refusal text.

The negative control replaces the reported SQL lock with the declared lock.
The mutant compiled, and the named lock diagnostic test failed with exit 101.
The only difference in the asserted values was the reported SQL lock.
The harness restored the exact source bytes, changed the modification time, and reran the test successfully.

Scoped Clippy passed under the repository lint policy.
It reported five library warnings, three test warnings, and one dependency warning, all at unchanged source lines.
Strict Clippy stopped at the five library warnings with exit 101.
`clippy-002/unchanged-warning-lines.json` records the source-line comparison against the base commit.
This comparison is not a separate baseline Clippy run.

These are local generator tests and lint results.
No database, cluster, full workspace, manifest, or inventory changes form part of this issue.

## Commands

Run the generation suite:

```sh
cargo test --locked --offline -p wamn-schema-generator --test generation -- --include-ignored
```

Run scoped Clippy:

```sh
cargo clippy --locked --offline -p wamn-schema-generator --test generation --no-deps
```

For each command, use `docs/perf/2026.09/effects-response/tools/capture.py` to retain its result and source hashes.
Use a new directory under `docs/perf/2026.09/generate-privileges/` for each run.
The checked-in command files contain the exact arguments and working directory.

After a passing generation run, run the negative control:

```sh
python3 docs/perf/2026.09/generate-privileges/tools/negative_control.py \
  --tree "$PWD" \
  --baseline-dir /home/kaalin/dev/wamn/docs/perf/2026.09/generate-privileges/generation-001 \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/generate-privileges/negative-control-002
```

The baseline directory must identify a passing run against the same source bytes.
`negative-control-001/result.json` records the original, mutated, and restored hashes with the two asserted values.
