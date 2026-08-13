#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.12.5"
readonly OUTCOME="default migrate-catalog stays additive-only while destructive planning remains internal to ops"
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
    allow-destructive-public-plan \
    emit-destructive-from-default-compiler \
    expose-destructive-flag \
    enable-ops-by-default \
    skip-copy-confirmation-read \
    drop-copy-authorization-consumption \
    bypass-copy-locked-window
}

load_mutation() {
  case "$1" in
    allow-destructive-dry-run)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="4f4404a87ee0ab11c42ca674ac9b40328aba3a7c118269becc5719082fbbbfbd"
      NEEDLE='        let plan = plan_error(plan_migration(&request))?;'
      REPLACEMENT='        let plan = plan_error(wamn_schema_control::ops::plan_target_reconciliation(&request))?;'
      GATE="orphan_guard_refuses_then_proceeds"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --features ops --test orphan_guard_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    allow-destructive-public-plan)
      TARGET="crates/schema/control/src/engine.rs"
      EXPECTED_SHA="9d8299fbb55c284b74345d18f54547fa9fd944e96925f6568bce752b60cf44e6"
      NEEDLE='    if c.destructive {
        return Err(MigrationError::Destructive(DestructiveMigration {
            operations: c
                .plan
                .destructive()
                .map(|operation| operation.summary.clone())
                .collect(),
        }));
    }
    let ddl_sql = c
        .plan
        .sql()
        .expect("the additive boundary rejected destructive operations above");'
      REPLACEMENT='    let ddl_sql = c.plan.ops_sql();'
      GATE="destructive_migration_is_not_a_public_capability"
      TEST_ARGV=(cargo test --locked --offline -p wamn-schema-control --features ops --test migrate "$GATE" -- --exact)
      ;;
    emit-destructive-from-default-compiler)
      TARGET="crates/schema/compiler/src/plan.rs"
      EXPECTED_SHA="036ea6fb3b6eeaf54e9eea09407ea0b666412674850907ccba3597293fc5465f"
      NEEDLE='        if self.is_destructive() {'
      REPLACEMENT='        if false {'
      GATE="dropped_column_is_gated_destructive"
      TEST_ARGV=(cargo test --locked --offline -p wamn-schema-compiler --features ops --test ddl "$GATE" -- --exact)
      ;;
    expose-destructive-flag)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="4f4404a87ee0ab11c42ca674ac9b40328aba3a7c118269becc5719082fbbbfbd"
      NEEDLE='    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,'
      REPLACEMENT='    #[arg(long, visible_alias = "confirm-with-backup")]
    pub skip_reconcile_replica_identity: bool,'
      GATE="mvp_migrate_catalog_has_no_destructive_override"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test verb_surface "$GATE" -- --exact)
      ;;
    enable-ops-by-default)
      TARGET="services/ctl/Cargo.toml"
      EXPECTED_SHA="81cdf5954c45936d665cbb305aaa86429468b04703e82cd49d5dbb470df6cb35"
      NEEDLE='default = []'
      REPLACEMENT='default = ["ops"]'
      GATE="mvp_dependency_tree_does_not_enable_ops"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test verb_surface "$GATE" -- --exact)
      ;;
    skip-copy-confirmation-read)
      TARGET="services/ctl/src/copy_project_env.rs"
      EXPECTED_SHA="f1ba7686186117809bd77855fe2fc9b08b4a0c59e2186b7ff92c32f8c88c128e"
      NEEDLE='wamn_control_provision::state::select_migration_confirmation_sql()'
      REPLACEMENT='"SELECT NULL::int, '\''backup-checkpoint-attested'\''::text, now(), session_user"'
      GATE="copy_authorization_wiring"
      TEST_ARGV=("$0" wiring-check)
      ;;
    drop-copy-authorization-consumption)
      TARGET="services/ctl/src/copy_project_env.rs"
      EXPECTED_SHA="f1ba7686186117809bd77855fe2fc9b08b4a0c59e2186b7ff92c32f8c88c128e"
      NEEDLE='            authorizations.remove(&cat.catalog_id),'
      REPLACEMENT='            None,'
      GATE="copy_authorization_wiring"
      TEST_ARGV=("$0" wiring-check)
      ;;
    bypass-copy-locked-window)
      TARGET="services/ctl/src/migrate_catalog.rs"
      EXPECTED_SHA="4f4404a87ee0ab11c42ca674ac9b40328aba3a7c118269becc5719082fbbbfbd"
      NEEDLE='        guard_target_reconciliation_window(authorization, locked_from, target.version)?;'
      REPLACEMENT='        let _ = (authorization, locked_from, target.version);'
      GATE="copy_authorization_wiring"
      TEST_ARGV=("$0" wiring-check)
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

assert_exact_anchor() {
  local target="$1"
  local needle="$2"
  local count
  count="$(TARGET="$target" NEEDLE="$needle" perl -0ne '
    BEGIN { $needle = $ENV{NEEDLE}; $count = 0 }
    $count += s/\Q$needle\E//g;
    END { print $count }
  ' "$target")"
  if [[ "$count" != 1 ]]; then
    echo "$target must contain the copy-authorization link exactly once; found $count" >&2
    return 1
  fi
}

check_copy_authorization_wiring() {
  assert_exact_anchor services/ctl/src/copy_project_env.rs \
    'wamn_control_provision::state::select_migration_confirmation_sql()'
  assert_exact_anchor services/ctl/src/copy_project_env.rs \
    '            authorizations.remove(&cat.catalog_id),'
  assert_exact_anchor services/ctl/src/migrate_catalog.rs \
    '        guard_target_reconciliation_window(authorization, locked_from, target.version)?;'
  echo "copy authorization wiring check clean"
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
    check_copy_authorization_wiring
    ;;
  wiring-check)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    check_copy_authorization_wiring
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
