#!/usr/bin/env bash
# The S0 authoring smoke's forged-token leg must be load bearing: a structurally
# valid token whose secret half is wrong by one hex digit has to be refused
# before the command runs. This mutant tells the script to read that leg's reply
# as an authorized success instead, and requires the live gate to fail.
#
# LIVE gate. It needs the deployed management surface and the two seeded
# credential files; it writes two draft revisions and two audit rows per run.
#
#   WAMN_AUTHORING_SMOKE_BASE_URL=http://HOST:PORT \
#   WAMN_AUTHORING_SMOKE_PRINCIPAL_A=/path/to/first.env \
#   WAMN_AUTHORING_SMOKE_PRINCIPAL_B=/path/to/second.env \
#     tools/gate-mutants/authoring-smoke-forged-token.sh run
set -euo pipefail

readonly TARGET="clients/authoring-client/scripts/smoke.mjs"
readonly EXPECTED_SHA="8c398eced30b6b8dae6fc87b8aa00a490ba8076b5fa74d85ef1b00b808bcb4ff"
readonly NEEDLE='credential: "forged", expect: "refused"'
readonly REPLACEMENT='credential: "forged", expect: "authorized"'
readonly GATE="authoring-leg-forged-token-status"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    exit 2
  }
  [[ "$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).read_text().count(sys.argv[2]))' "$TARGET" "$NEEDLE")" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
  for variable in WAMN_AUTHORING_SMOKE_BASE_URL WAMN_AUTHORING_SMOKE_PRINCIPAL_A \
    WAMN_AUTHORING_SMOKE_PRINCIPAL_B; do
    [[ -n "${!variable:-}" ]] || {
      echo "$variable must name the live surface or a credential file" >&2
      exit 2
    }
  done
}

run_gate() {
  node "$TARGET" \
    --base-url "$WAMN_AUTHORING_SMOKE_BASE_URL" \
    --principal "$WAMN_AUTHORING_SMOKE_PRINCIPAL_A" \
    --principal "$WAMN_AUTHORING_SMOKE_PRINCIPAL_B"
}

run_mutant() (
  local backup_dir backup restored_sha exit_code output
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rm -f "$backup_dir/gate.log"
    rmdir "$backup_dir"
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  MUTANT_TARGET="$TARGET" MUTANT_NEEDLE="$NEEDLE" MUTANT_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["MUTANT_TARGET"]); d=p.read_text(); p.write_text(d.replace(os.environ["MUTANT_NEEDLE"], os.environ["MUTANT_REPLACEMENT"], 1))'
  output="$backup_dir/gate.log"
  set +e
  run_gate >"$output" 2>&1
  exit_code=$?
  set -e
  cat "$output"
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED forged-token-accepted" >&2; exit 1; }
  grep -q "check=$GATE" "$output" || {
    echo "gate failed for the wrong reason: $GATE was not the failing check" >&2
    exit 1
  }
  echo "KILLED forged-token-accepted gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
