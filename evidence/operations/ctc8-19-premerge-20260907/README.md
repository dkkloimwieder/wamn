# Human membership proof, 2026-09-07

This directory preserves the pre-merge evidence for `wamn-ctc8.19`.
The successful deployed run used source commit `6c3b12f851b86c417161d07cf18b1d4ed8109aa0`.
The [HTTP receipt](canonical-6c3b12f8/membershipproof.receipt) records seven passing cases and successful fixture cleanup.
The [journey receipt](canonical-6c3b12f8/membershipproof-journey.receipt) records that source commit.
The [cleanup receipt](canonical-6c3b12f8/cleanup.receipt) records removal of the disposable cluster, containers, and image tags.

The [Job record](canonical-6c3b12f8/membershipproof-job.json) and [Pod record](canonical-6c3b12f8/membershipproof-pod.json) show one completed Job and exit code zero.
The image records retain the source labels and the matching digests from all three nodes.
The [deployed log](deployed-6c3b12f8.log) also records the thirteen-route integration test: one passed in 25.21 seconds.
The [runner exit status](deployed-6c3b12f8.exit) is zero.

## Scope and limitations

The [workspace log](workspace-sweep-6c3b12f8.log) records 1,845 passes and 54 failures at the same source commit.
Its [exit status](workspace-sweep-6c3b12f8.exit) is 101.
The failing names match the preceding sweep.
Missing live inputs and unrelated failures remain recorded on `wamn-0h0g.15.137`.
Self-skipped tests do not prove live behavior.
The sweep includes ignored tests and excludes two source-regeneration commands:

```bash
RUSTC_WRAPPER=sccache cargo test --workspace --locked --offline --no-fail-fast \
  -- --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema
```

The [first failed run](failed-6bc8a2a3/failure-job-membershipproof.log) remains intact.
Its fixture used `purchase_order.get` instead of the canonical operation grant `wamn-receiving:purchase-order/get@1.0.0`.
Commit `895bf1fe` corrected that fixture before the successful run.

## Integration and file integrity

Merge commit `af5e0aed` incorporates main `0a2b89d0` after the proof ran.
That integration changes pilot scripts and documentation, not membership code or its build and deployment inputs.
The measurements above describe the recorded source commit, not a new run after integration.

The 67 captured files retain their original bytes.
Paths inside historical logs describe the original capture locations and remain unchanged.
The repository now owns the evidence instead of the cache directory.

From this directory, make sure that every captured file matches its recorded hash:

```bash
sha256sum --check raw-files.sha256
```

Run future proofs from a clean worktree with a new repository evidence directory.
Use the [MEMBERSHIP-HTTP command](https://github.com/dkkloimwieder/wamn/blob/6c3b12f851b86c417161d07cf18b1d4ed8109aa0/docs/operations/build-and-test.md#L1187).
