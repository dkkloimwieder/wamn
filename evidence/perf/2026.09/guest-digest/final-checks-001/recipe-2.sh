set -euo pipefail
GUEST_PROFILE_ROOT="$(git rev-parse --show-toplevel)"
GUEST_PROFILE_RUN="$(date -u +%Y%m%dT%H%M%SZ)-$$"
GUEST_PROFILE_SCRATCH="$HOME/.cache/wamn-lanes/guest-profile-$GUEST_PROFILE_RUN"
GUEST_PROFILE_EVIDENCE="$GUEST_PROFILE_ROOT/docs/perf/$(date -u +%Y.%m)/guest-digest-profile/$GUEST_PROFILE_RUN"
mkdir -p -- "$HOME/.cache/wamn-lanes" "$GUEST_PROFILE_EVIDENCE"
mkdir -- "$GUEST_PROFILE_SCRATCH"
git rev-parse HEAD > "$GUEST_PROFILE_EVIDENCE/commit.txt"
for profile in m1 proof; do
  CARGO_TARGET_DIR="$GUEST_PROFILE_SCRATCH/$profile" RUSTC_WRAPPER= \
    ./tools/build-components build-only "$profile" \
    > "$GUEST_PROFILE_EVIDENCE/$profile.json" \
    2> "$GUEST_PROFILE_EVIDENCE/$profile-build.log"
done
CARGO_TARGET_DIR="$GUEST_PROFILE_SCRATCH/test-target" \
WAMN_DIGEST_PROFILE_M1_PLAN="$GUEST_PROFILE_EVIDENCE/m1.json" \
WAMN_DIGEST_PROFILE_PROOF_PLAN="$GUEST_PROFILE_EVIDENCE/proof.json" \
  cargo test --locked --offline -p wamn-proof-conformance --test guest_workspace_closure \
  one_commit_built_under_two_profiles_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture \
  > "$GUEST_PROFILE_EVIDENCE/gate.log" 2>&1
rm -r -- "$GUEST_PROFILE_SCRATCH"
