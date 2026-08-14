#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-4u7p.38"
readonly OUTCOME="request cannot regain the model-owned interface exemption reserved for call-flow"

readonly TARGET="crates/catalog/model/src/lib.rs"
readonly MUTATION="request-regains-model-owned-interface-exemption"
readonly TEST="request_nodes_require_an_explicit_pinned_interface"
readonly NEEDLE='const MODEL_OWNED_NODES: [&str; 1] = ["call-flow"];'
readonly REPLACEMENT='const MODEL_OWNED_NODES: [&str; 2] = ["call-flow", "request"];'
readonly EXPECTED_SHA="fcf17cb4ef7ea651a1a32f7f32f0f03d20d56f8f414fa7dbd1a2f75f9e3e1de8"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name a unique debug target directory" >&2
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
  count="$(env NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path("crates/catalog/model/src/lib.rs").read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
  echo "CHECKED id=$MUTATION target=$TARGET sha256=$actual"
}

gate() {
  cargo test --locked -p wamn-catalog --test identity "$TEST" -- --exact
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
  env NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path("crates/catalog/model/src/lib.rs"); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
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
