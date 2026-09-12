#!/usr/bin/env bash
# Run the actual runner guard against retained logs without starting services.
set -euo pipefail

source_root=$(cd -- "${1:?pass the source checkout}" && pwd -P)
evidence_root=${2:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/results}
test ! -e "$evidence_root"
mkdir -- "$evidence_root"
runner=$source_root/tools/receiving-cluster-journey-run
session=$source_root/docs/perf/2026.09/ctc8-15-3-host-sessions/deployed-007
cutover=$source_root/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-009/journey
route=production_two_package_release_serves_all_thirteen_pat_routes

git -C "$source_root" rev-parse HEAD
sha256sum "$runner"
guard=$(sed -n '/^assert_one_test_ran() {$/,/^}$/p' "$runner")
test "$(printf '%s\n' "$guard" | grep -Fc 'assert_one_test_ran() {')" = 1
printf '%s\n' "$guard" | sha256sum
eval "$guard"

positive_count=0
positive() {
  local log=$1 selected=$2
  sha256sum "$log"
  assert_one_test_ran "$log" "$selected"
  positive_count=$((positive_count + 1))
  printf 'positive name=%s result=pass\n' "$selected"
}
negative() {
  local name=$1 selected=$2 status=0
  (assert_one_test_ran /dev/stdin "$selected") >"$evidence_root/$name.log" 2>&1 || status=$?
  test "$status" = 1
  printf 'negative name=%s expected_exit=1 actual_exit=%s result=pass\n' "$name" "$status" \
    | tee -a "$evidence_root/negative-controls.receipt"
}

positive "$session/production-route.log" "$route"
positive "$session/session-host-fixture.log" production_receiving_session_host_fixture
positive "$session/session-nested.log" production_nested_session_call_preserves_original_caller
positive "$cutover/production-route.log" "$route"
awk '{ print }' "$cutover/production-route.log" | negative wrong-name nonexistent_selected_test
awk '{ print; print }' "$cutover/production-route.log" | negative duplicate "$route"
awk '!/^test result:/' "$cutover/production-route.log" | negative missing-summary "$route"
sed 's/test result: ok\. 1 passed; 0 failed;/test result: ok. 0 passed; 0 failed;/' \
  "$cutover/production-route.log" | negative zero-test "$route"
sed 's/test result: ok\. 1 passed; 0 failed;/test result: FAILED. 0 passed; 1 failed;/' \
  "$cutover/production-route.log" | negative failed-test "$route"
negative_count=$(wc -l <"$evidence_root/negative-controls.receipt")
test "$positive_count" = 4
test "$negative_count" = 5

bash -n "$runner"
printf 'syntax result=pass\n'
"$runner" >"$evidence_root/normal-dry-run.log"
dry_count=1
for mode in fresh-only-proof session-host-proof membershipproof measure-startup throughput fresh-auth-bench; do
  "$runner" "--$mode" >"$evidence_root/$mode-dry-run.log"
  dry_count=$((dry_count + 1))
done
for mode in session-host-proof membershipproof throughput; do
  status=0
  "$runner" --fresh-only-proof "--$mode" >"$evidence_root/conflict-$mode.log" 2>&1 || status=$?
  test "$status" = 2
  printf 'conflict mode=%s expected_exit=2 actual_exit=%s result=pass\n' "$mode" "$status"
done

for available in true false; do
  stage=host-session-reachable
  if [[ "$available" == false ]]; then stage=host-session-unreachable; fi
  sha256sum "$session/$stage.log" "$session/$stage-process-continuity.json"
  receipt="HOST_SESSION_PROOF result=pass hosts=2 jwks_available=$available"
  test "$(grep -Fxc -- "$receipt" "$session/$stage.log")" = 1
  jq -e '.result == "pass" and .hosts == 2 and (.processes | length == 2)' \
    "$session/$stage-process-continuity.json" >/dev/null
done
receipt='HOST_SESSION_NESTED result=pass credential_kind=session invocations=2 fixture_release=3 manifest_format=1'
test "$(grep -Fxc -- "$receipt" "$session/session-nested.log")" = 1
for publication in flow-http materializer; do
  sha256sum "$cutover/$publication-push.json"
  jq -e '.success == true and .data.success == true and (.data.digest | test("^sha256:[0-9a-f]{64}$"))' \
    "$cutover/$publication-push.json" >/dev/null
done
git -C "$source_root" diff --check -- tools/receiving-cluster-journey-run
sha256sum "$runner"
printf 'RUNNER_CONTROLS result=pass positive=%s negative=%s dry_modes=%s conflicts=3 session_receipts=3 continuity_records=2 upstream_publications=2\n' \
  "$positive_count" "$negative_count" "$dry_count"
