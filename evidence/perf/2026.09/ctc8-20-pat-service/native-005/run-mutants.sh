#!/usr/bin/env bash
set -euo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-005
readonly lane=/home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910
cd "$lane"
export RUSTC_WRAPPER=
test -z "$(git status --porcelain)"
git rev-parse HEAD >"$evidence_dir/source.commit"

restore_operator() {
  apply_patch <<'PATCH'
*** Begin Patch
*** Update File: services/identity/src/pat.rs
@@
-    if !operator && false {
+    if !operator {
*** End Patch
PATCH
}
restore_redirect() {
  apply_patch <<'PATCH'
*** Begin Patch
*** Update File: services/ctl/src/pat_client.rs
@@
-            .redirect(reqwest::redirect::Policy::limited(2))
+            .redirect(reqwest::redirect::Policy::none())
*** End Patch
PATCH
}
restore_grants() {
  apply_patch <<'PATCH'
*** Begin Patch
*** Update File: crates/control/provision/src/identity_issuer.rs
@@
-pub const IDENTITY_ISSUER_PAT_INSERT_COLUMNS: [&str; 6] = [
-    "revoked_at",
+pub const IDENTITY_ISSUER_PAT_INSERT_COLUMNS: [&str; 5] = [
*** End Patch
PATCH
}

run_pg_test() (
  set -euo pipefail
  local label=$1 package=$2 test_file=$3 selector=$4 env_prefix=$5
  local container="wamn-ctc8-20-mutant-${label}-20260910-005"
  if docker container inspect "$container" >/dev/null 2>&1; then exit 2; fi
  docker run --detach --name "$container" --label wamn.proof=ctc8-20-native-005 \
    -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_system \
    -p 127.0.0.1::5432 postgres:18 >"$evidence_dir/$label.container"
  cleanup() {
    docker logs "$container" >"$evidence_dir/$label.postgres.log" 2>&1
    docker rm --force --volumes "$container" >"$evidence_dir/$label.cleanup.log" 2>&1
  }
  trap cleanup EXIT
  local port ready=0
  port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container")
  [[ "$port" =~ ^[0-9]+$ ]]
  local url="postgresql://postgres:probe@127.0.0.1:$port/wamn_system"
  for attempt in {1..60}; do
    if psql "$url" -X -v ON_ERROR_STOP=1 -Atqc 'SELECT 1' >"$evidence_dir/$label.ready.log" 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  test "$ready" = 1
  set +e
  env "${env_prefix}_PG_URL=$url" "${env_prefix}_ALLOW_SCHEMA_RESET=1" \
    cargo test --locked --offline -p "$package" --test "$test_file" "$selector" \
    -- --include-ignored --exact --nocapture --test-threads=1 >"$evidence_dir/$label.log" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" >"$evidence_dir/$label.exit"
  test "$status" = 101
  rg --fixed-strings --quiet "    $selector" "$evidence_dir/$label.log"
  rg --quiet '^test result: FAILED\. 0 passed; 1 failed;' "$evidence_dir/$label.log"
)

sha256sum services/identity/src/pat.rs >"$evidence_dir/operator.before.sha256"
trap restore_operator EXIT
apply_patch <<'PATCH'
*** Begin Patch
*** Update File: services/identity/src/pat.rs
@@
-    if !operator {
+    if !operator && false {
*** End Patch
PATCH
run_pg_test operator wamn-identity pat_issuance operator_pat_issuance_over_https WAMN_PAT_ISSUANCE
restore_operator
trap - EXIT
sha256sum --check "$evidence_dir/operator.before.sha256" >"$evidence_dir/operator.restore.log"

sha256sum services/ctl/src/pat_client.rs >"$evidence_dir/redirect.before.sha256"
trap restore_redirect EXIT
apply_patch <<'PATCH'
*** Begin Patch
*** Update File: services/ctl/src/pat_client.rs
@@
-            .redirect(reqwest::redirect::Policy::none())
+            .redirect(reqwest::redirect::Policy::limited(2))
*** End Patch
PATCH
set +e
cargo test --locked --offline -p wamn-ctl --lib \
  pat_client::tests::https_issuance_does_not_redirect_retry_or_accept_large_bodies \
  -- --exact --nocapture --test-threads=1 >"$evidence_dir/redirect.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" >"$evidence_dir/redirect.exit"
test "$status" = 101
rg --fixed-strings --quiet '    pat_client::tests::https_issuance_does_not_redirect_retry_or_accept_large_bodies' "$evidence_dir/redirect.log"
rg --quiet '^test result: FAILED\. 0 passed; 1 failed;' "$evidence_dir/redirect.log"
restore_redirect
trap - EXIT
sha256sum --check "$evidence_dir/redirect.before.sha256" >"$evidence_dir/redirect.restore.log"

sha256sum crates/control/provision/src/identity_issuer.rs >"$evidence_dir/grants.before.sha256"
trap restore_grants EXIT
apply_patch <<'PATCH'
*** Begin Patch
*** Update File: crates/control/provision/src/identity_issuer.rs
@@
-pub const IDENTITY_ISSUER_PAT_INSERT_COLUMNS: [&str; 5] = [
+pub const IDENTITY_ISSUER_PAT_INSERT_COLUMNS: [&str; 6] = [
+    "revoked_at",
*** End Patch
PATCH
run_pg_test grants wamn-control-provision identity_issuer_live \
  scoped_issuer_grants_and_generation_retirement_execute_on_postgres WAMN_IDENTITY_ISSUER
restore_grants
trap - EXIT
sha256sum --check "$evidence_dir/grants.before.sha256" >"$evidence_dir/grants.restore.log"
git diff --exit-code >"$evidence_dir/source.restore.log"
printf 'operator=detected redirect=detected excess_grant=detected source=restored\n' >"$evidence_dir/result"
