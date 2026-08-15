#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.12.43"
readonly OUTCOME="the projection writer and pre-effect reset preserve exact authority and fence evidence"
readonly CAMPAIGN="run-projection-writer"
readonly BEAD="wamn-0h0g.12.43"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the dedicated debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    omit-reset-advisory-fence \
    omit-fresh-effect-reclassification \
    weaken-reset-owner-fence \
    weaken-reset-expiry-fence \
    weaken-reset-generation-fence \
    clear-state-before-private-reset \
    allow-inherited-rogue-node-runs-authority \
    allow-projection-only-connected-generation
}

load_mutation() {
  local id="$1"
  case "$id" in
    omit-reset-advisory-fence)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="f14e263f0693a9f9f15167f4f242a4f8768da7867f5b00c56a24a0afc2f61f1d"
      NEEDLE='serialize_effect_intent_sql().as_str(), &[&fence.run_id]'
      REPLACEMENT='effect_run_is_runnable().text(), &[&fence.run_id]'
      GATE="effect_writer::tests::projection_reset_is_exact_expired_fenced_and_deletes_only_node_runs"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    omit-fresh-effect-reclassification)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="f14e263f0693a9f9f15167f4f242a4f8768da7867f5b00c56a24a0afc2f61f1d"
      NEEDLE='if effect_row.get::<_, bool>(0) {'
      REPLACEMENT='if false {'
      GATE="native_effect_writer_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --test effect_writer_live "$GATE" -- --ignored --exact --nocapture)
      ;;
    weaken-reset-owner-fence)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="f14e263f0693a9f9f15167f4f242a4f8768da7867f5b00c56a24a0afc2f61f1d"
      NEEDLE='       AND q.lease_owner IS NOT DISTINCT FROM $2::text
       AND q.lease_expires_at IS NOT DISTINCT FROM $3::text::timestamptz
       AND q.lease_generation IS NOT DISTINCT FROM $4::bigint'
      REPLACEMENT='       AND true
       AND q.lease_expires_at IS NOT DISTINCT FROM $3::text::timestamptz
       AND q.lease_generation IS NOT DISTINCT FROM $4::bigint'
      GATE="effect_writer::tests::projection_reset_is_exact_expired_fenced_and_deletes_only_node_runs"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    weaken-reset-expiry-fence)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="f14e263f0693a9f9f15167f4f242a4f8768da7867f5b00c56a24a0afc2f61f1d"
      NEEDLE='       AND q.lease_owner IS NOT DISTINCT FROM $2::text
       AND q.lease_expires_at IS NOT DISTINCT FROM $3::text::timestamptz
       AND q.lease_generation IS NOT DISTINCT FROM $4::bigint'
      REPLACEMENT='       AND q.lease_owner IS NOT DISTINCT FROM $2::text
       AND true
       AND q.lease_generation IS NOT DISTINCT FROM $4::bigint'
      GATE="effect_writer::tests::projection_reset_is_exact_expired_fenced_and_deletes_only_node_runs"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    weaken-reset-generation-fence)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="f14e263f0693a9f9f15167f4f242a4f8768da7867f5b00c56a24a0afc2f61f1d"
      NEEDLE='       AND q.lease_owner IS NOT DISTINCT FROM $2::text
       AND q.lease_expires_at IS NOT DISTINCT FROM $3::text::timestamptz
       AND q.lease_generation IS NOT DISTINCT FROM $4::bigint'
      REPLACEMENT='       AND q.lease_owner IS NOT DISTINCT FROM $2::text
       AND q.lease_expires_at IS NOT DISTINCT FROM $3::text::timestamptz
       AND true'
      GATE="effect_writer::tests::projection_reset_is_exact_expired_fenced_and_deletes_only_node_runs"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    clear-state-before-private-reset)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs"
      EXPECTED_SHA="7f3e063444c181eb76f3317ffabfcdde0408a81ce3ca2c805aa872f5f0604ffc"
      NEEDLE='if has_projection {'
      REPLACEMENT='if has_projection && false {'
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture)
      ;;
    allow-inherited-rogue-node-runs-authority)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="aad9cc3e4ee2cefdee48c7083f7f4d3d64bd4203055823cfc8ee12482a78600b"
      NEEDLE="                                 AND actor.rolname !~ '^pg_' \\
                                 AND actor.rolname NOT IN ('wamn_app', '{SCENARIO_AUTHOR_ROLE}', \\
                                                           '{EFFECT_WRITER_ROLE}', '{RUN_PROJECTION_WRITER_ROLE}') \\
                                 AND actor.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \\
"
      REPLACEMENT="                                 AND false \\
"
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    allow-projection-only-connected-generation)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="aad9cc3e4ee2cefdee48c7083f7f4d3d64bd4203055823cfc8ee12482a78600b"
      NEEDLE="                    IS DISTINCT FROM ARRAY[ \\
                         'wamn_effect_writer', 'wamn_run_projection_writer']::text[] \\
"
      REPLACEMENT="                    IS DISTINCT FROM ARRAY[ \\
                         'wamn_run_projection_writer']::text[] \\
"
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  fi
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; path=pathlib.Path(os.environ["TARGET"]); data=path.read_text(); path.write_text(data.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  if [[ "$mutant_sha" == "$EXPECTED_SHA" ]]; then
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  fi
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  run_gate
  exit_code=$?
  set -e
  if [[ $exit_code -eq 0 ]]; then
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  fi
  echo "KILLED id=$id gate=$GATE exit_code=$exit_code"
)

check_campaign() {
  local id
  while IFS= read -r id; do
    load_mutation "$id"
    assert_precondition
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' \
      "$id" "$GATE" "$TARGET" "$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() {
  echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2
}

case "${1:-}" in
  list) mutation_ids ;;
  check) check_campaign ;;
  green)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_green "$2"
    ;;
  green-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_green "$id"; done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids)
    ;;
  *) usage; exit 2 ;;
esac
