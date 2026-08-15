#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.12.71"
readonly OUTCOME="shared flow-invocation outcome hints never replace the mandatory truth poll"
readonly TARGET="crates/platform/runtime/src/flow_invocation.rs"
readonly EXPECTED_SHA="ae7c8e7f9eac854d3dce767d849e46e08641aaff89b8a454e1f49dc39e4d37c6"
readonly NEEDLE='released(self.backend.poll(&self.config.tenant_id, &run_id).await?)?
            .map(to_invoke_result)
            .transpose()'
readonly REPLACEMENT='Ok(None)'
readonly GATE="flow_invocation::tests::lost_notification_falls_back_to_the_final_bounded_poll"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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
    return 2
  }
  count="$(MUTATION_NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{MUTATION_NEEDLE}\E/g; END { print $count }' "$TARGET")"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain exactly one final-poll mutation anchor (found $count)" >&2
    return 2
  }
}

gate() {
  CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-runtime \
    --lib "$GATE" -- --exact
}

run_mutant() (
  local backup_dir backup exit_code restored mutant_sha
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
  MUTATION_NEEDLE="$NEEDLE" MUTATION_REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{MUTATION_NEEDLE}\E/$ENV{MUTATION_REPLACEMENT}/' "$TARGET"
  mutant_sha="$(sha256)"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation anchor did not change $TARGET" >&2
    exit 2
  }
  set +e
  gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED final-poll-removed" >&2
    exit 1
  }
  echo "KILLED final-poll-removed owner=$OWNER outcome=$OUTCOME gate=$GATE mutant_sha256=$mutant_sha"
)

case "${1:-}" in
  check)
    check
    echo "flow-invocation listener mutation anchor check clean"
    ;;
  green)
    check
    gate
    ;;
  run-all)
    run_mutant
    ;;
  *)
    echo "usage: $0 check | green | run-all" >&2
    exit 2
    ;;
esac
