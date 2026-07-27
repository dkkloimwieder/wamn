# Amended findings

## Overall disposition

The submitted findings are substantially correct. I would retain all five high findings, sharpen several formulations, move one medium item down, fold two medium items into their related high findings, and add four high findings plus one product blocker.

The reviewed `main` remains at `96f4ca4`. The repository contains **47 members in the native root workspace and 18 members in the component workspace—65 members across two workspaces**, not 65 packages in one workspace. Because the root is a virtual workspace without `default-members`, unqualified root Cargo commands select its 47 members; they do not select the excluded component workspace. ([GitHub][1])

Three other wording corrections should be made up front:

* `[package.metadata.wamn]` is one reasonable implementation of executable architecture classification, but it is not itself the requirement. A central `architecture/packages.toml`, generated classification, or path-derived policy could be equally valid.
* `.gitignore` does not control the Docker build context. The Docker defect is that the final stages copy `components/target/...` directly from the build context, while `.dockerignore` does not exclude that directory and no component-builder stage produces those artifacts.
* The observation that the local `flowrunner.wasm` predates its source commit necessarily comes from the local checkout because that artifact is untracked. I can verify the non-hermetic build structure remotely, but not the local file timestamp.

---

# Revised high findings

## H1. The gate of record does not prove the product scenario worker

**Accepted and strengthened. Treat as a release blocker for the scenario-worker feature.**

`suiteexec-job.yaml` runs `wamn-gates testkitbench`, not `wamn-scenario-worker`. `testkitbench` contains a second stored-suite executor that loads suites, provisions schemas, instantiates `ExecutionHost`, drives runs, captures results, and evaluates assertions. The product worker independently implements the same orchestration. A passing gate therefore proves the duplicate integration implementation, not the shipped service. ([GitHub][2])

The implementations also differ materially:

* The gate owns superuser schema provisioning and uses the administrative session for database assertions.
* The product worker expects caller-provisioned execution schemas and reads results through its application connection.
* The product worker uses `ScenarioScheduler`; the gate’s stored-suite path has its own execution lifecycle.
* Terminal status and captured observations are derived differently.

The required end state is:

```text
scenarios/application
  authoritative suite orchestration

scenario-worker
  process/CLI shell
  → scenarios/application

integration proofs
  provision external fixtures
  launch scenario-worker binary or image
  observe its public report
```

Focused tests may call the application package directly. The deployed gate must launch the actual service artifact. The duplicate stored-suite executor should be deleted or reduced to fixture preparation and process invocation.

---

## H2. Docker images are non-hermetic and can ship stale or locally modified Wasm

**Accepted and upgraded from “stale risk” to a source-provenance and release-integrity failure.**

The Docker builder compiles only native root-workspace packages. The executor, scenario-worker, and gates stages subsequently copy `flowrunner.wasm` and numerous other artifacts directly from `components/target` in the caller’s build context. They are not copied from a component build stage and are not built by the Dockerfile. ([GitHub][3])

This gives two bad outcomes:

```text
clean checkout without prebuilt component artifacts
  → image build fails at COPY

dirty checkout with locally built component artifacts
  → image build succeeds with whatever bytes happen to be present
```

The executor and scenario-worker may contain byte-identical `flowrunner.wasm` files, but that proves only equality with each other. It does not prove correspondence to the checked-out component source.

The fix is a dedicated component-builder stage:

```dockerfile
FROM rust:... AS component-builder

COPY components/Cargo.toml components/Cargo.lock ./components/
COPY components/... ./components/...

RUN rustup target add wasm32-wasip2
RUN cd components \
 && cargo build --locked --release --target wasm32-wasip2
```

All production and proof stages should then use:

```dockerfile
COPY --from=component-builder \
  /build/components/target/wasm32-wasip2/release/flowrunner.wasm \
  /components/flowrunner.wasm
```

Additionally:

* Add `/components/target` to `.dockerignore`.
* Build JCO and WAC-composed artifacts in pinned builder stages.
* Use `--locked`.
* Record the source commit, component lockfile digest, toolchain versions, and artifact hashes as image metadata.
* Add a clean-checkout image build to CI.
* Refuse release artifacts that contain a component not produced by the same build invocation.

This should be fixed before relying on any image-level proof.

---

## H3. Scenario schema identity and isolation are not structurally safe

**Accepted, with a scope correction.**

The product worker’s local `is_bare_identifier` validator checks only the character grammar. It does not enforce PostgreSQL’s 63-byte identifier limit, while the canonical `wamn_pg_core::Identifier` does. Separately, `EphemeralSchemaProvisioner` accepts a raw `&str` and interpolates it into privileged `DROP SCHEMA`, `CREATE SCHEMA`, and `GRANT` statements. ([GitHub][4])

The product worker does not currently call the provisioner itself; it expects pre-provisioned schemas. The defect spans the **scenario execution contract and provisioning boundary**:

* Two distinct template results can differ only after byte 63.
* PostgreSQL can truncate both to the same physical identifier.
* The worker can believe the cases are isolated because their Rust strings differ.
* The database can resolve both to the same schema.
* The privileged provisioner API remains unsafe by construction because it accepts an untyped raw name.

The fix should not be another local length check. Introduce a canonical type and lease:

```rust
struct ScenarioSchemaName(wamn_pg_core::Identifier);

struct ScenarioSchemaLease {
    execution_id: ScenarioExecutionId,
    schema: ScenarioSchemaName,
    generation: u64,
    expires_at: Timestamp,
    ownership_token: SecretToken,
}
```

Requirements:

* Generate physical names server-side.
* Keep the ordinal or digest inside the first 63 bytes.
* Quote identifiers through `pg-core`.
* Make `provision_case` and `drop_case` accept the validated type, not `&str`.
* Have the worker consume a verified schema lease rather than an arbitrary template.
* Test byte-length boundaries, multibyte names, ordinal collisions, reused leases, concurrent executions, and truncation collisions.

---

## H4. The dependency and state-ownership architecture is not executable

**Accepted, with corrected terminology.**

The finding is not “65 packages lack a particular metadata table.” The finding is:

> The repository has no comprehensive machine-executed model of package roles, target classes, allowed dependency directions, or durable-state ownership.

The root and component manifests describe 47 and 18 members respectively, but do not encode the proposed architecture rules. The current system-tier build script performs literal manifest and source-text scans; it does not inspect Cargo’s resolved graph and cannot reliably detect aliases, transitive dependencies, build dependencies, target-specific dependencies, or feature-induced edges. ([GitHub][5])

The package classification may live in either:

```text
[package.metadata.wamn]
role = "service"
target-class = "native"
```

or:

```text
architecture/packages.toml
```

A central manifest may be preferable because it provides one reviewable system model and avoids duplicating boilerplate across 65 manifests.

An architecture checker should consume `cargo metadata` separately for both workspaces and enforce at least:

```text
production service → production service        forbidden
contract/model → service                       forbidden
guest-safe → native-only                       forbidden
standard-nodes → data-api                      forbidden
production executor → scenario runtime         forbidden
system proof → service library                 forbidden
non-runtime package → wash-runtime              restricted
new durable state without an owner declaration forbidden
```

It must distinguish normal, build, development, and target-specific edges rather than pretending they have identical consequences.

State ownership should be encoded in a second machine-readable manifest:

```toml
[[state]]
object = "wamn_run.runs"
semantic_owner = "run-state"
migration_owner = "run-state"
writers = ["executor"]
readers = ["executor", "dispatcher", "ctl"]
```

CI should detect:

* Unclassified packages.
* Forbidden resolved edges.
* New tables without ownership entries.
* Write SQL outside the declared owner.
* Multiple migration owners.
* Drift between generated and checked-in schema artifacts.

The current string-scanning system guard should be removed once the resolved-graph checker subsumes it.

---

## H5. The verification runbook contains false-green and invalid commands

**Accepted and reframed as verification-integrity debt, not ordinary documentation drift.**

`wamn-host` is now a binary-only package, but `docs/build-and-test.md` continues to direct runtime and plugin tests to commands such as:

```bash
cargo test -p wamn-host guard_
cargo test -p wamn-host live_scs_off_server_fails_checkout_closed
cargo test -p wamn-host --lib plugins::wamn_postgres::tests
```

Filtered `cargo test` commands can succeed with zero matching tests, while `--lib` cannot target a package that has no library target. A green shell command therefore does not necessarily provide the evidence the runbook claims. ([GitHub][6])

The “active navigation/deployment documentation contains moved paths” medium finding belongs here; it is another manifestation of the same ownership failure.

The correction should be structural:

* Replace free-form verification recipes with named `xtask`, `just`, or script entry points.
* Make each named gate own the exact package, target, and test expression.
* Fail when a test selection matches zero tests.
* Validate all referenced manifests, paths, binary targets, and Kubernetes resources.
* Execute the documented fast-path commands in CI.
* Generate the relevant command snippets from the executable task definitions where practical.

A command that exits zero but runs no relevant test should count as a failed verification contract.

---

## H6. Scenario scheduling violates its earliest-first and isolation contract

**New high finding.**

The scheduler documentation and in-memory test say that it advances to the earliest deadline and leaves later work parked. The real SQL first selects the minimum future `available_at`, but its nudge statement then changes **every future global queue row for the current tenant** to `now()`. It is not scoped to the selected deadline, current run, or scenario execution. The product worker executes these SQL constants without supplying such a scope. ([GitHub][7])

Consequences include:

* A `+24h` row can become runnable during the `+1h` step.
* One scenario can wake another scenario’s queued work.
* Stale rows in a reused schema can be driven unexpectedly.
* The in-memory earliest-first unit test proves behavior the SQL backend does not implement.

There is also a clock-domain defect. The virtual clock starts from a deterministic fixed epoch, while `run_queue.available_at` is anchored to PostgreSQL `now()`. The scheduler advances the virtual clock to the database’s real wall-clock timestamp, so a supposedly deterministic scenario can depend on the calendar date on which it ran. ([GitHub][7])

Required design:

```text
scenario_execution_id or run_id scopes every scheduler query
next_deadline = earliest deadline for that execution
release only rows:
  belonging to that execution
  with available_at <= selected deadline
one authoritative logical scenario clock
```

A live PostgreSQL test must cover:

* Deadlines at `+1h` and `+24h`.
* Two concurrent scenario executions under one tenant.
* Stale rows from a previous execution.
* Identical captured time on different calendar dates.
* Replay producing byte-identical time observations.

---

## H7. Egress assertions currently grant network capability

**New high finding. Severity becomes security-critical once scenarios are tenant-authored.**

The scenario worker extracts authorities from `ExactlyThese` and `Includes` assertions, passes them to `RecordingEgress::expect`, and uses that expectation set as the runtime allowlist. `RecordingEgress` then forwards allowed requests through the real outgoing HTTP handler. An observational assertion therefore expands what the flow may contact. ([GitHub][4])

That collapses three independent concepts:

```text
authorization — may this execution contact the destination?
fixture       — what deterministic response should it receive?
assertion     — what call should be observed?
```

An assertion must never grant authority.

Introduce distinct product types:

```text
ScenarioEgressPolicy
  deny-all
  stub-only
  trusted-live-forward

EgressFixture
  request matcher
  deterministic response or failure
  delay behavior

EgressAssertion
  observation only
```

For any trusted live forwarding, effective permission must be:

```text
deployment outer policy
∩ flow-declared allowed hosts
∩ trusted scenario policy
```

Assertions may narrow or inspect that result; they must not broaden it. The default scenario mode should deny or stub all external egress.

---

## H8. Database assertions are executable SQL, not read-only observations

**New high finding if stored scenarios are tenant-authored; otherwise medium-high. The trust level must be explicit.**

`DbState` stores an arbitrary SQL string. The product worker executes that string directly on its application connection without visibly establishing a read-only transaction, statement timeout, row limit, or result-size limit. The integration gate executes the stored assertion query through its superuser session. A statement described as an assertion can therefore mutate state—for example through `UPDATE ... RETURNING` or a writable common table expression. ([GitHub][8])

At minimum, arbitrary observation SQL requires:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = ...;
-- execute one statement
ROLLBACK;
```

It also needs:

* A dedicated non-superuser observer identity.
* Maximum rows and bytes.
* Typed result-shape failures.
* No multi-statement execution.
* Explicit author-trust classification.

The preferred product design is a structured observation model that reuses `entity-access`:

```text
entity/table
predicate
selected fields
ordering
expected result
```

The gate should never evaluate stored scenario SQL with its provisioning superuser.

---

## H9. The custom-node invocation boundary remains operationally unsafe

**New to this submitted list, but still open from the preceding review. Treat as a production blocker for `node-host`.**

`node-host` still uses a hand-written HTTP parser. Its accept loop processes one connection to completion before accepting another; it supports keep-alive; header and body reads have no visible size or time limits; invalid `Content-Length` becomes zero; fixed bodies are allocated directly from the supplied length; and chunked bodies grow without an aggregate limit. Authentication occurs after the body has been collected. ([GitHub][9])

Authentication also remains fail-open when no current key is configured and `require_key` is false. The production-labelled deployment leaves strict key enforcement commented out. ([GitHub][9])

The runtime adds another failure mode:

* One mutex serializes all node invocations.
* The Wasmtime epoch deadline is effectively disabled.
* `deadline_ms` is passed to the guest but is not a host-enforced timeout.
* A hanging guest can hold the component, credential grant, HTTP request, and service capacity indefinitely.
* Streamed output is silently converted into an inline debug-shaped value.
* The configuration cache is unbounded. ([GitHub][10])

Required remediation:

```text
Hyper/Axum or equivalent HTTP implementation
POST /run validation
header/body limits
read/idle/whole-request timeouts
bounded connection and invocation queues
host-owned Wasmtime interruption deadline
outbound-request timeouts
fail-closed production authentication
bounded configuration cache
explicit streamed-payload-unsupported error
startup/readiness/liveness probes
```

Sequential execution of one warm node instance can be intentional. Sequential acceptance of unbounded network connections is not.

---

# Product blocker

## P1. Generic HTTP flow ingress is still absent

The current production ingress tree contains only `api-gateway`; there is no generic `flow-http` component for invoking an arbitrary active flow. ([GitHub][11])

This remains the principal product gap. It should not be hidden by the restructuring backlog.

The ingress should depend on a stable invocation boundary, not directly on standard nodes, run-state SQL, or runtime plugins:

```text
HTTP caller
  → authenticate and resolve trusted route
  → versioned flow-invocation contract
  → executor
  → durable run state
  → terminal HTTP result or asynchronous run handle
```

The current architectural cleanup is valuable precisely because it should make this boundary possible.

---

# Revised medium findings

## M1. Dependency centralization remains incomplete — **medium-high**

Accepted. The exact count of 11 Wasmtime declarations comes from the local audit; the repository visibly repeats the same Wasmtime Git revision in several native packages, while the root centralizes only a small subset of common dependencies and the component workspace has no `[workspace.dependencies]`. The current lockfile resolving one revision means this is a latent drift risk, not an observed split-runtime failure. ([GitHub][5])

Centralize source URLs, revisions, and baseline versions independently in both workspaces. Keep package-specific feature selection local, because guest, runtime, and proof packages need different feature closures.

## M2. Test packages import deployable service libraries — **medium**

Accepted with scope clarification.

The production graph passes the service-as-leaf rule. The test and support graph does not. This is not equivalent to a production service depending on another service, but it means tests can bypass the actual process boundary and reusable application behavior still lives inside service packages. The integration suite’s broad internal composition is also what enabled the duplicate scenario executor. ([GitHub][12])

The desired rule is:

```text
integration tests → application/core packages allowed
system tests      → deployed artifacts and public clients
tests             → service implementation libraries exceptional
```

## M3. Root `default-members` remains absent — **medium**

Accepted, corrected to **47 root-workspace members**, not all 65 members across both workspaces. Cargo uses all members of a virtual workspace when `default-members` is absent. ([GitHub][5])

Defaults should include the ordinary production libraries and services, while POCs and heavyweight proof suites remain explicit. Full CI should continue to run `--workspace`, plus the separate component workspace.

## M4. Conformance owns reusable proof support and has a broad closure — **medium**

Accepted. The issue is not that every conformance proof must be dependency-light. Some runtime conformance tests legitimately need Wasmtime and runtime adapters. The inversion is that schema-drift helpers owned by the conformance package are reused by integration and system tiers.

Move reusable drift and fixture-control code into:

```text
test-support/schema-drift
```

or, preferably, into the package that owns the schema contract. Conformance should be a consumer of proof support, not its architectural owner.

## M5. `crates/execution/host` is an emerging sink — **medium-high**

Strengthen this beyond naming ambiguity.

`wamn-execution-host` is described as a shared composition core, but it also owns the long-lived run-worker loop, NATS doorbells, polling/backoff, production egress policy, telemetry, memory accounting, shutdown behavior, and component instantiation. It is shared with the scenario worker despite containing serving-specific infrastructure. ([GitHub][13])

The likely final split is:

```text
execution/runtime-host
  instantiate flowrunner
  invoke run-next
  drain one execution
  capability injection

services/executor
  NATS doorbell
  polling/backoff
  supervision
  production metrics and lifecycle
```

The ambiguity with `services/host` is a symptom; mixed responsibility is the actual problem.

## M6. Rollout readiness is not real application readiness — **medium-high**

The runner manifest explicitly states that there is no readiness endpoint and claims `minReadySeconds` plus `maxUnavailable: 0` protects against a bad database Secret. Without an application readiness probe, process survival is not evidence that the worker can access its database or claim runs. ([GitHub][14])

Add distinct startup, readiness, and liveness semantics. The same applies to `node-host`, whose manifest also lacks application probes. ([GitHub][15])

## M7. `wamn-api` has native development dependencies — **downgrade to low**

I would not retain this as a medium architectural finding.

Native Tokio/Postgres dependencies confined to `[dev-dependencies]` do not contaminate the shipped guest closure. This is acceptable unless the architecture explicitly requires the package’s entire test target to remain guest-safe. Moving live database tests to the integration tier may improve classification purity, but this is hygiene rather than a current boundary failure.

---

# Findings that should be folded together

Two submitted medium findings should not remain separate:

* **The system-tier text guard** belongs under H4, because it is the inadequate current implementation of architecture enforcement.
* **Moved navigation and deployment paths** belong under H5, because they are part of the runbook’s verification-integrity failure.

This avoids double-counting symptoms.

---

# Confirmed passes

I would retain all the submitted passes, with two explicit caveats:

1. They are **snapshot properties**, not durable architecture guarantees, until H4 is fixed.
2. “Shipped Wasm guest closures contain no native dependencies” currently proves the **manifest/resolved source graph**. H2 means the Wasm bytes placed into images are not yet proven to correspond to that source graph.

The retained passes are therefore:

* Physical service/crate/component/test/POC organization matches the restructuring vocabulary.
* The production graph has no deployable-to-deployable dependency.
* Principal contract packages do not depend on services or runtimes.
* Source-defined guest closures remain free of native database/runtime dependencies.
* Standard nodes use `entity-access`, not the complete data API.
* The executor excludes scenario and repository test-support code.
* The scenario worker includes scenario runtime explicitly.
* Production packages exclude repository-only test support.
* The two-workspace target boundary remains intact.
* `wamn-run-worker` and `wamn-gates` remain documented compatibility naming exceptions.

These are genuine achievements. They should not be discounted merely because enforcement and artifact provenance remain incomplete.

---

# Recommended remediation order

## 1. Repair the evidence chain first

Before trusting further image or scenario results:

1. Build every Wasm component inside Docker.
2. Make the build clean-checkout reproducible.
3. Make the actual scenario-worker the gate-of-record.
4. Repair false-green runbook commands.

Until these are done, a green image or scenario gate is weaker evidence than it appears.

## 2. Repair scenario isolation and authority

Next:

1. Introduce typed schema names and execution leases.
2. Scope scheduler queries to one scenario execution.
3. Establish one logical clock.
4. Separate egress policy, fixtures, and assertions.
5. Make database observations read-only and bounded.

## 3. Make the architecture enforceable

Then:

1. Add resolved dependency-graph fitness checks.
2. Add state-owner declarations and SQL ownership checks.
3. Add `default-members`.
4. Centralize dependency revisions.
5. Remove text-scanning guards superseded by the architecture checker.

## 4. Finish the production boundaries

Finally—but not indefinitely later:

1. Harden `node-host`.
2. Add real readiness and startup checks.
3. Split serving-loop responsibilities out of `execution-host`.
4. Build the generic HTTP flow ingress.

The amended central conclusion is:

> The physical restructure is succeeding, but the repository’s proof chain, scenario authority model, and operational boundaries have not caught up. The next work should make the new architecture executable and trustworthy—not add another layer of directory movement.

[1]: https://github.com/dkkloimwieder/wamn/commits/main "Commits · dkkloimwieder/wamn · GitHub"
[2]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/deploy/gates/suiteexec-job.yaml "raw.githubusercontent.com"
[3]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/Dockerfile "raw.githubusercontent.com"
[4]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/services/scenario-worker/src/lib.rs "raw.githubusercontent.com"
[5]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/Cargo.toml "raw.githubusercontent.com"
[6]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/services/host/Cargo.toml "raw.githubusercontent.com"
[7]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/scenarios/runtime/src/scheduler.rs "raw.githubusercontent.com"
[8]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/scenarios/model/src/assertion.rs "raw.githubusercontent.com"
[9]: https://github.com/dkkloimwieder/wamn/blob/main/services/node-host/src/main.rs "https://github.com/dkkloimwieder/wamn/blob/main/services/node-host/src/main.rs"
[10]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/platform/node-runtime/src/lib.rs "https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/platform/node-runtime/src/lib.rs"
[11]: https://github.com/dkkloimwieder/wamn/tree/main/components/ingress "https://github.com/dkkloimwieder/wamn/tree/main/components/ingress"
[12]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/tests/integration/src/testkitbench.rs "https://raw.githubusercontent.com/dkkloimwieder/wamn/main/tests/integration/src/testkitbench.rs"
[13]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/execution/host/Cargo.toml "https://raw.githubusercontent.com/dkkloimwieder/wamn/main/crates/execution/host/Cargo.toml"
[14]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/deploy/platform/runner.yaml "https://raw.githubusercontent.com/dkkloimwieder/wamn/main/deploy/platform/runner.yaml"
[15]: https://raw.githubusercontent.com/dkkloimwieder/wamn/main/deploy/platform/serve-node.yaml "https://raw.githubusercontent.com/dkkloimwieder/wamn/main/deploy/platform/serve-node.yaml"
