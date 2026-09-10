# Native descriptor startup proof

Owner: `wamn-0ct2.1`. Date: 2026-09-10.

The host and executor call upstream `raise_descriptor_limit()` before native descriptor-derived resources.
Both processes record the effective descriptor limit and the native default connection ceilings.
The process-global call stays outside the shared engine builder.
Explicit connection configuration retain their existing behavior.
The change fills an embedding omission and removes no predecessor code.

## Source and artifacts

The initial source base is `6228917e74744eadb69a7422a912a7ea319e3925`.
The final clean proof uses `de444a64ebc862815f0c604058dcc83785a292b1`, based on Receiving boundary `d853e132cb7056cf0822bca115f1f215ddc11e4b`.
The [final inputs](final-001/inputs.json) and [proof](final-001/proof/result.json) retain that source identity and both rebuilt binary hashes.
The exact added service code is identified by SHA-256 in [build inputs](build-001/inputs.json).
The proof also records every selected source hash and each binary hash in its result.
Upstream remains unmodified wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
[PR #5529](https://github.com/wasmCloud/wasmCloud/pull/5529) requires each embedder to call the helper.
No guest artifacts, interfaces, source pins or Cargo dependencies change in A.

## Executed proof

The focused debug build of `wamn-host` and `wamn-executor` passed.
[Build inputs](build-001/inputs.json) retain the command, toolchain and source hashes.
[Build result](build-001/result.json) records exit 0.
The runtime emits 16 existing warnings, with no new service warning.

| Run | Passed | Failed | Skipped | Result |
|---|---:|---:|---:|---|
| [Initial subprocess proof](proof-001/result.json) | 4 | 0 | 0 | Pass |
| [Helper omission mutant](mutation-001/proof/result.json) | 2 | 2 | 0 | Expected failure |
| [Rebuilt restored source](restore-001/proof/result.json) | 4 | 0 | 0 | Pass |
| [Final clean source after rebase](final-001/proof/result.json) | 4 | 0 | 0 | Pass |

Both binaries start each case in a separate subprocess.
The raise cases start with soft 256 and hard 4096.
The helper leaves soft 4096, with native defaults of 2048 guest connections and 1024 ingress connections.
The hard-ceiling cases retain soft and hard 256, with native defaults of 128 and 256.
Each process reaches the deliberate local CA refusal and exits 1 before release pulls or network setup.
The parent retains its original soft 1024 and hard 524288 in every run.

The mutant replaces the helper call with `Some(256_usize)` while retaining the native default calculations.
It fails `host_raises_low_soft` and `executor_raises_low_soft` for the lower effective limit and connection ceilings.
The two hard-ceiling cases continue to pass.
The [mutation receipt](mutation-001/result.json) records both named failures and the exact source restoration.
The [restore receipt](restore-001/result.json) records the fresh rebuild and passing rerun.
The [final receipt](final-001/result.json) records a successful incremental build and four passes after the Receiving landing.

## Scope and remaining deviations

The hard-ceiling cases do not inject a failing system call.
These tests prove startup and native defaults on Linux, with no engine, workload, network or deployment proof claim.
No benchmark ran because this change affects startup resource limits.
The unmodified 2.9 helper and its default calculations supply the comparison values.
The existing build guide contains the `[NATIVE-A]` reproduction recipe.
The ledger records native adoption and corrects its stale PostgreSQL comparison.
Ledger row 4 remains unchanged until B2 lands its mechanism and policy together.
