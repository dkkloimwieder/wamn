#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-4q3c.12"
readonly OUTCOME="runner egress rejects forbidden resolved destination addresses"

readonly CAMPAIGN="runner-egress-address"
readonly BEAD="wamn-4q3c.12"
readonly TARGET="deploy/gates/runner-connection-egress.yaml"
readonly EXPECTED_SHA="deb05f78f8aefb2ed2261a98b37162cad633bd600190541898acc6e15212acfa"
readonly NEEDLE='              app: serve-echo'
readonly REPLACEMENT=$'              app: serve-echo\n        - podSelector:\n            matchLabels:\n              app: egress-escape'
readonly KIND_CLUSTER="${KIND_CLUSTER_NAME:-wamn}"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

kubectl_executable=${KUBECTL:-kubectl}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(python3 -c \
    'import pathlib, sys; print(pathlib.Path(sys.argv[1]).read_text().count(sys.argv[2]))' \
    "$TARGET" "$NEEDLE")"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  }
}

prepare_cluster() {
  "$kubectl_executable" -n wamn-system apply -f deploy/platform/runner-netpol.yaml
  "$kubectl_executable" -n wamn-system apply -f "$TARGET"
  local -a kind_nodes
  mapfile -t kind_nodes < <(kind get nodes --name "$KIND_CLUSTER")
  [[ ${#kind_nodes[@]} -gt 0 ]] || {
    echo "kind cluster $KIND_CLUSTER has no nodes" >&2
    return 2
  }
  docker exec "${kind_nodes[0]}" \
    nft list table inet kindnet-network-policies >/dev/null || {
      echo "kindnet NetworkPolicy nftables table is absent; gate is invalid" >&2
      return 2
    }
  "$kubectl_executable" -n wamn-system apply -f deploy/gates/serve-echo.yaml
  "$kubectl_executable" -n wamn-system apply -f deploy/gates/egress-escape.yaml
  "$kubectl_executable" -n wamn-system rollout status deployment/serve-echo --timeout=120s
  "$kubectl_executable" -n wamn-system rollout status deployment/egress-escape --timeout=120s
}

run_gate() {
  local verdict
  verdict="$(mktemp)"
  if tools/kubernetes-gate-run \
      --manifest deploy/gates/credproof-job.yaml \
      --verdict-record "$verdict" \
      --namespace wamn-system \
      --timeout-secs 300 \
      --job '{"name":"credproof","container":"credproof","expectation":"positive","exit_code":0,"image":"wamn-gates:dev","log_contains":"address escape blocked"}'; then
    rm -f "$verdict"
    return 0
  else
    local status=$?
    rm -f "$verdict"
    return "$status"
  fi
}

green() {
  assert_precondition
  prepare_cluster
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD target=$TARGET gate=credproof"
  run_gate
}

run_mutant() (
  local backup_dir backup restored_sha mutant_sha mutant_exit
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    "$kubectl_executable" -n wamn-system apply -f "$TARGET" >/dev/null
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  python3 -c \
    'import pathlib, sys; p=pathlib.Path(sys.argv[1]); s=p.read_text(); p.write_text(s.replace(sys.argv[2], sys.argv[3], 1))' \
    "$TARGET" "$NEEDLE" "$REPLACEMENT"
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation did not change $TARGET" >&2
    exit 3
  }
  prepare_cluster
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=admit-address-escape target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha gate=credproof"
  set +e
  run_gate
  mutant_exit=$?
  set -e
  [[ $mutant_exit -ne 0 ]] || {
    echo "SURVIVED id=admit-address-escape gate=credproof" >&2
    exit 1
  }
  echo "KILLED id=admit-address-escape gate=credproof exit_code=$mutant_exit"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) green ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
