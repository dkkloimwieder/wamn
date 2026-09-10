set -euo pipefail
GUEST_REPRO_ROOT="$(git rev-parse --show-toplevel)"
GUEST_REPRO_COMMIT="$(git rev-parse HEAD)"
GUEST_REPRO_RUN="$(date -u +%Y%m%dT%H%M%SZ)-$$"
GUEST_REPRO_SCRATCH="$HOME/.cache/wamn-lanes/guest-repro-$GUEST_REPRO_RUN"
GUEST_REPRO_EVIDENCE="$GUEST_REPRO_ROOT/docs/perf/$(date -u +%Y.%m)/guest-digest-repro/$GUEST_REPRO_RUN"
mkdir -p -- "$HOME/.cache/wamn-lanes" "$GUEST_REPRO_EVIDENCE"
mkdir -- "$GUEST_REPRO_SCRATCH"
printf '%s\n' "$GUEST_REPRO_COMMIT" > "$GUEST_REPRO_EVIDENCE/commit.txt"
for side in a b; do
  mkdir -- "$GUEST_REPRO_SCRATCH/$side"
  git worktree add --detach "$GUEST_REPRO_SCRATCH/$side/tree" "$GUEST_REPRO_COMMIT"
  (
    cd "$GUEST_REPRO_SCRATCH/$side/tree"
    CARGO_TARGET_DIR="$GUEST_REPRO_SCRATCH/$side/target" RUSTC_WRAPPER= \
      ./tools/build-components build-only m1 \
      > "$GUEST_REPRO_EVIDENCE/$side-plan.json" \
      2> "$GUEST_REPRO_EVIDENCE/$side-build.log"
    CARGO_TARGET_DIR="$GUEST_REPRO_SCRATCH/$side/target" RUSTC_WRAPPER= \
      ./tools/build-components virtualize-only "$GUEST_REPRO_EVIDENCE/$side-plan.json" \
      > "$GUEST_REPRO_EVIDENCE/$side-virtualize.log" 2>&1
  )
done
CARGO_TARGET_DIR="$GUEST_REPRO_SCRATCH/test-target" \
WAMN_DIGEST_REPRO_A="$GUEST_REPRO_SCRATCH/a/target/virtualized/std-empty-environment" \
WAMN_DIGEST_REPRO_B="$GUEST_REPRO_SCRATCH/b/target/virtualized/std-empty-environment" \
  cargo test --locked --offline -p wamn-proof-conformance --test guest_workspace_closure \
  one_commit_built_in_two_checkouts_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture \
  > "$GUEST_REPRO_EVIDENCE/gate.log" 2>&1
git worktree remove "$GUEST_REPRO_SCRATCH/a/tree"
git worktree remove "$GUEST_REPRO_SCRATCH/b/tree"
rm -r -- "$GUEST_REPRO_SCRATCH"
