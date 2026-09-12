#!/usr/bin/env bash
# Run only at the source boundary that the owner authorizes.
# Each step uses the normal generator, SQLx preparation, or component builder.
set -euo pipefail
tree=/home/kaalin/.cache/wamn-lanes/receiving-update-projection-20260910
root=/home/kaalin/dev/wamn
evidence=$root/docs/perf/2026.09/receiving-update-projection
capture=$evidence/tools/capture.py

[[ ${1-} == --apply ]] || {
  echo 'Prepared sequence only. Read this script, then pass --apply at the authorized boundary.'
  exit 0
}

# generator-build-001 already builds this example into the exclusive lane cache.
test -x "$tree/target/debug/examples/materialize_package"
python3 "$root/docs/perf/2026.09/generated-tui-integration/tools/materialize.py" \
  --tree "$tree" --evidence-root "$root" --evidence-dir "$evidence/materialize-001"

python3 "$capture" --tree "$tree" --evidence-dir "$evidence/sqlx-001" -- \
  python3 "$evidence/tools/sqlx_pg18.py" --tree "$tree" --evidence-dir "$evidence/sqlx-001"

python3 "$capture" --tree "$tree" --evidence-dir "$evidence/m1-001" -- \
  env CARGO_TARGET_DIR="$tree/target" tools/build-components m1

python3 "$capture" --tree "$tree" --evidence-dir "$evidence/remint-capture-001" -- \
  python3 "$evidence/tools/remint.py" --tree "$tree" \
  --build-evidence "$evidence/m1-001" --evidence-dir "$evidence/remint-001"

python3 "$capture" --tree "$tree" --evidence-dir "$evidence/sqlx-offline-001" -- \
  env SQLX_OFFLINE=true cargo test --locked --offline \
  -p wamn-proof-conformance --test receiving_sqlx_verifier
