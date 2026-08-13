#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.9.5"
readonly OUTCOME="default migrate-catalog remains additive-only with no impact or destructive override"
readonly CAMPAIGN="migrate-catalog-additive"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the debug target directory" >&2
  exit 2
fi
command -v perl >/dev/null || {
  echo "perl is required for byte-exact mutation replacement" >&2
  exit 2
}

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    allow-destructive-dry-run \
    allow-destructive-apply \
    expose-destructive-flag \
    expose-impact-shell-by-default
}

load_mutation() {
  case "$1" in
    allow-destructive-dry-run)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="bc5eee0c591a9af6ff144f5ac05c9d5c0dd745e8e79ec61146afce34bb1207f7"
      NEEDLE='            expected_base: args.base,
            confirm: Confirmation::None,'
      REPLACEMENT='            expected_base: args.base,
            confirm: Confirmation::ConfirmedWithBackup,'
      GATE="orphan_guard_refuses_then_proceeds"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test orphan_guard_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    allow-destructive-apply)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="bc5eee0c591a9af6ff144f5ac05c9d5c0dd745e8e79ec61146afce34bb1207f7"
      NEEDLE='        args.base,
        Confirmation::None,
        true,'
      REPLACEMENT='        args.base,
        Confirmation::ConfirmedWithBackup,
        true,'
      GATE="orphan_guard_refuses_then_proceeds"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test orphan_guard_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    expose-destructive-flag)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="bc5eee0c591a9af6ff144f5ac05c9d5c0dd745e8e79ec61146afce34bb1207f7"
      NEEDLE='    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,'
      REPLACEMENT='    #[arg(long, visible_alias = "confirm-with-backup")]
    pub skip_reconcile_replica_identity: bool,'
      GATE="mvp_migrate_catalog_has_no_destructive_override"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test verb_surface "$GATE" -- --exact)
      ;;
    expose-impact-shell-by-default)
      TARGET="services/ctl/src/lib.rs"
      EXPECTED_SHA="6e336c9166276b9b7d342e571503d5728b4ca9ec0c0be9e9a2057f7f0ebe1d7a"
      NEEDLE='#[cfg(feature = "ops")]
pub mod impact_report;'
      REPLACEMENT='pub mod impact_report;'
      GATE="impact_effect_shell_is_ops_only"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test verb_surface "$GATE" -- --exact)
      ;;
    *)
      echo "unknown mutant: $1" >&2
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
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" perl -0ne '
    BEGIN { $needle = $ENV{NEEDLE}; $count = 0 }
    $count += s/\Q$needle\E//g;
    END { print $count }
  ' "$TARGET")"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once; found $count" >&2
    return 2
  fi
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e '
    BEGIN {
      $needle = $ENV{NEEDLE};
      $replacement = $ENV{REPLACEMENT};
      $count = 0;
    }
    $count += s/\Q$needle\E/$replacement/g;
    END { exit($count == 1 ? 0 : 1) }
  ' "$TARGET"
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  load_mutation "$1"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN id=$1 gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  local id="$1"
  local backup_dir backup restored_sha mutant_sha exit_code
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

  echo "MUTANT campaign=$CAMPAIGN id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
  list)
    mutation_ids
    ;;
  check)
    check_campaign
    ;;
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
  *)
    usage
    exit 2
    ;;
esac
