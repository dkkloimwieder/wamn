#!/usr/bin/env bash
set -euo pipefail

runner=tools/receiving-cluster-journey-run
bash -n "$runner"
"$runner" --receiving-correctness
"$runner" --fresh-only-proof

combined_status=0
"$runner" --receiving-correctness --fresh-only-proof || combined_status=$?
[[ "$combined_status" == 2 ]]

source <(sed -n '/^assert_one_test_ran() {$/,/^}$/p' "$runner")
receiving_log=/home/kaalin/dev/wamn/docs/perf/2026.09/receiving-correctness/live-003/journey/receiving-correctness.log
identity_log=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-4-fresh-only/deployed-006/session-nested.log
assert_one_test_ran "$receiving_log" production_receiving_command_histories ''
assert_one_test_ran "$identity_log" production_nested_fresh_only_requires_pat_and_observes_revocation

wrong_prefix_status=0
(assert_one_test_ran "$receiving_log" production_receiving_command_histories wrong::) || wrong_prefix_status=$?
[[ "$wrong_prefix_status" == 1 ]]
printf 'Merged runner modes and exact-test receipt checks pass. No builds or live calls ran.\n'
