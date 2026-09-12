The guest build command now selects every guest with `all`. This result belongs to `wamn-47wm.5.5`.

Source commit `4fd7ca12e898e6440f605aecc085b254805de6f3` renames the argument and its current callers in eight files. Application selection still reads the named manifests. Each guest retains one Cargo invocation. Guest manifests, generated files, operation tokens, and component names remain unchanged.

[All six commands](results.json) exited successfully. Bash accepted the build script, and Python accepted the maintained comparison helper. [Seven existing tests](classification.json) passed in 276.548 seconds, including the initial compile. The selector tests exercised exact Cargo arguments, new workspace members, declared application components, artifact handling, and refused arguments. The two guest byte comparisons require separate builds and remained ignored.

[The all selection](command-4.stdout) reported `apps` and `apps/platform/no-std` as its workspace roots. [The Receiving selection](command-5.stdout) reported `apps`. These commands used actual Cargo metadata. Both application test crates compiled for all native targets in 173.066 seconds. Their cluster cases did not execute in this run.

The commands used Rust 1.98.0, locked offline dependencies, two Cargo jobs, and this branch's own target directory. [The source record](source-commit.json) matches the tested bytes to the commit. Source bytes, modes, and HEAD stayed unchanged during execution. Existing compiler warnings remain in the logs. The parent step retains the integrated workspace sweep.

The publication map records each original file's hash, size, and mode. All copied records retain their exact bytes.
