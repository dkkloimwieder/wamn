#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-2jdm.5.10"
readonly OUTCOME="connection-binding resolution in effect authority is scoped to the run's own catalog version"

# RE-ANCHORED by wamn-0h0g.15.122 (owner ruling), absorbed into wamn-0h0g.15.66's
# lane. The subject is ALIVE, it moved: the authoritative-identity SQL left
# components/execution/flowrunner/src/lib.rs for the host-side effect-authority
# statement, and the alias changed from `d` to `binding`. Re-anchored against the
# post-wamn-0h0g.15.66 predicate so it is not re-anchored twice.
#
# The killing test is a source-text guard over the statement, not a behavioural
# one: the live effect-authority proof seeds exactly one connection binding, so it
# observes no difference when the version predicate is dropped. A behavioural
# negative for catalog-version scoping is filed as follow-up, not faked here.
readonly TARGET="crates/platform/runtime/src/plugins/wamn_postgres/claims.rs"
readonly MUTATION="runner-accepts-wrong-catalog-version"
readonly TEST="plugins::wamn_postgres::claims::tests::effect_authority_resolves_the_current_binding_and_draft_grant"
readonly NEEDLE="AND binding.catalog_version = r.catalog_version \\"
readonly REPLACEMENT="AND true \\"
# Still the digest of the FORMER target. wamn-0h0g.15.22 owns re-deriving every
# baseline in this wave from the file the script actually points at; no lane
# re-baselines mid-wave.
readonly EXPECTED_SHA="1bc244bb02f9a872e2e9ba204972683a0cf521058a79d693f41279ece75cb2c4"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi

sha256() {
  sha256sum "$TARGET" | cut -d ' ' -f 1
}

check() {
  local actual count
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    exit 2
  }
  count="$(env TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
  echo "CHECKED id=$MUTATION target=$TARGET sha256=$actual"
}

gate() {
  cargo test --locked -p wamn-runtime --lib "$TEST" -- --exact
}

run_mutant() (
  local backup_dir backup restored mutant_exit
  check
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored="$(sha256)"
    rm -f "$backup"
    rmdir "$backup_dir"
    [[ "$restored" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  env TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
  set +e
  gate
  mutant_exit=$?
  set -e
  [[ $mutant_exit -ne 0 ]] || {
    echo "SURVIVED id=$MUTATION" >&2
    exit 1
  }
  echo "KILLED id=$MUTATION exit_code=$mutant_exit"
)

case "${1:-}" in
  check) check ;;
  green) check; gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
