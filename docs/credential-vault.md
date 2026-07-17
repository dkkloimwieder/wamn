# Credential vault (5.9)

How a flow uses a stored secret without ever holding it. Flows reference a
credential **by name**; the secret material lives in a per-project vault the
host owns, is resolved **lazily at node execution**, and exists only inside
the executing node's context — never in flow data, node config, run/node_run
records, or a sibling node's context.

Shipped by wamn-17o. Contract: `docs/wamn-node.wit` (`interface credentials`,
frozen 0.1 at 5.4 — this is its first host implementation).

## The chain

```
flow JSON                      runner (wamn-host run-worker)          guest (flowrunner)
─────────                      ─────────────────────────────          ──────────────────
credentials:                   WAMN_CREDENTIALS_FILE                  Dispatch.credential
  - name: notify-token   ───►  {project: {name: secret}}              (the DECLARED name)
nodes:                         mounted from a K8s Secret;                    │
  - id: notify                 wamn_credentials plugin,                      ▼
    type: http-request         project-scoped, audit-logged   ◄───  CapsCtx.credential()
    credential: notify-token          `get(handle)`                 (wamn:node/credentials)
                                                                            │
                                                                            ▼
                                                              http-request sends it as the
                                                              `authorization` header (or the
                                                              config `credential-header`)
```

1. **Reference (5.1, already shipped)** — `wamn-flow` `CredentialRef{name,
   kind?, description?}` + `node.credential`. `validate()` rejects an
   undeclared reference. No secret material ever enters the graph.
2. **Host resolution** — the `wamn_credentials` plugin
   (`crates/wamn-host/src/plugins/wamn_credentials.rs`) implements the frozen
   `wamn:node/credentials` `get(handle) -> result<string, credential-error>`.
   Resolution is **project-scoped**: the executing component's project is a
   host-injected claim (`set_project` / the `wamn.project` workload config —
   the wamn:postgres tenant/project pattern), never guest input. Every `get`
   is **audit-logged** (`wamn::credentials` target: component, project,
   handle, outcome — never the value). v1 source: a mounted static file
   (`--credentials-file` / `WAMN_CREDENTIALS_FILE`, JSON
   `{project: {name: secret}}` — the `WAMN_PG_PROJECTS_FILE` pattern; the
   material still lives in a K8s Secret at the deploy layer,
   `deploy/runner-credentials.example.yaml`). A live per-Secret K8s read is a
   follow-up sharing wamn-5x0.1's client.
3. **Per-dispatch scoping (the containment invariant, structural)** — the
   flowrunner builds a **fresh** `CapsCtx` per node dispatch carrying ONLY
   that node's declared credential name (`Dispatch.credential`). The SDK
   facade `NodeCtx::credential()` deliberately takes **no name**: a node can
   only read its own declared credential; a node that declared none gets
   `NotGranted` **locally**, without the vault ever being asked. The secret
   is materialized only inside the executing node's call.
   **Host-enforced grant (cjv.3)** — `CapsCtx` is a *guest-side* facade a
   direct-import custom node (wamn-bd5) could bypass, so the host enforces the
   frozen contract's per-execution grant too: `credentials.get(handle)` returns
   `not-granted` for any name outside the executing component's granted set,
   and an unregistered project fails **closed** (never a fail-open default).
   The granted set is registered per component:
   - the trusted, compiled-in **flow-runner** declares its per-**run** grant
     (the flow's declared `credentials`) via the trusted
     `wamn:runner/credentials` `set-granted` channel — linked ONLY into its
     world — right after loading the flow; per-node scoping still rides
     `CapsCtx`, so `get` is bounded by both the run's grant and the node's
     declaration;
   - a **custom node** (wamn-bd5) — a separate per-invocation component that
     imports `wamn:node/credentials` directly and never gets the trusted
     channel — will be granted its exact declared name(s) host-side by the
     runner before invocation.
4. **Consumption** — the standard `http-request` node resolves its declared
   credential and sends it as the `authorization` header (config
   `credential-header` overrides the header name; an explicit config header
   of the same name wins — the trace-context rule). Error mapping is
   mechanical: `not-found` → terminal `credential-not-found` (config-shaped);
   `unavailable` → retryable `credential-unavailable` (the store);
   `not-granted` → no credential declared, the request proceeds bare.

## Error semantics (host side)

| Condition | WIT error | Node taxonomy |
|---|---|---|
| Handle not in the component's granted set (cjv.3) | `not-granted` | terminal |
| Unregistered project — fail-closed (cjv.3) | `not-granted` | terminal |
| Granted name, no source configured at all (no file) | `unavailable` | retryable |
| Granted name, source present but name absent | `not-found` | terminal |
| Node declared no credential | (guest-local) `not-granted` | proceed bare |

`not-granted` precedes any lookup, so an ungranted `get` never learns whether
the secret exists. The direct-import bypass is proven closed by the
`credprobe` gate (`crates/wamn-gates/src/credprobe.rs` +
`components/fixtures/cred-probe`): a fixture that imports `wamn:node/credentials`
directly — exactly as a custom node would — is granted a narrow set host-side,
and an ungranted / unregistered-project `get` is refused over the real WIT
boundary.

A **missing** credentials file is a warn + empty vault (the Secret mounts
`optional` in `deploy/runner.yaml`, so a credential-less project deploys
cleanly); a **malformed** file is a hard startup error.

## Run-worker egress (wired with 5.9)

The vault's consumer is outbound `wasi:http`, and the run-worker's store
previously had **no outgoing handler at all** — an outbound call trapped
("http client not available") and poisoned the instance. 5.9 wires
`RunnerEgress` (`run_worker.rs`): the fork's `check_allowed_hosts` gate over
`DefaultOutgoingHandler` (which also stamps the 9.2 trace context).
**Fail-closed**: the allowlist comes from `--allowed-hosts` /
`WAMN_ALLOWED_HOSTS` (repeatable; `host[:port]`, `scheme://host`, `*.domain`,
`*`), and EMPTY = DENY-ALL — an http-request to an unlisted host fails
`egress-denied` (a clean denial, never a trap). Host-level governance only;
per-flow allowlists are the fqg.11 refinement.

## The gate (`credproof`)

`crates/wamn-gates/src/credproof.rs` — the ladderproof shape: a pure DB
client seeds ONE manual run of `deploy/cred/notify.flow.json`
(`in → http-request{credential: notify-token} → transform{status} → respond`)
and waits for the **separately-deployed** run-worker to drive it against
serve-echo. serve-echo reflects a **one-way FNV-1a digest** of the
`authorization` header it received (never the raw value), so the two 5.9
acceptance halves are both provable:

* **Delivery** — the http node's recorded output carries serve-echo's
  reflected digest, and it equals `fnv1a(secret)`. The flow names the
  credential only by reference, so a matching digest at the target can only
  have come from the vault.
* **Containment** — because the witness is a digest, the scan is **total**:
  the secret substring must appear in NO recorded row — `runs.input_json` /
  `result_json` / `state_json` / `fail_reason`, the registered `graph_json`,
  and every `node_runs` row's input/output/error.

Gate of record: `deploy/credproof-job.yaml` against `deploy/runner.yaml` (with
`deploy/runner-credentials.example.yaml`) + `deploy/serve-echo.yaml`.
Verification commands: [build-and-test.md](build-and-test.md) § *[5.9]*.

## Host-enforced grant (cjv.3)

cjv.3 hardened the seam before custom nodes ship: `get` is now grant-enforced
host-side, and `project_for` fails **closed** (see § *Per-dispatch scoping* and
the error table above). The plugin holds a per-component granted set
(`grants`), a trusted `wamn:runner/credentials.set-granted` channel lets the
compiled-in flow-runner declare its per-run grant (the host cannot observe the
single long-lived component's per-node boundary otherwise), and the direct-import
threat is proven closed by the `credprobe` gate. cjv.3 **unblocks wamn-bd5**: the
runner will register a custom node's exact declared grant host-side
(`set_granted_credentials`) before invoking it, with a get-only linker (never the
trusted channel). It does **not** wire that per-invocation registration — that
lands with bd5, when the custom-node invocation path exists.

## Scope boundary

5.9 owns: by-name resolution + the WIT seam host implementation + per-dispatch
scoping + http-request consumption + the mounted-file source + the gate.
cjv.3 adds: host-side grant enforcement + fail-closed project.
NOT here: live per-Secret K8s reads (follow-up, shares wamn-5x0.1's client);
per-flow egress allowlists (wamn-fqg.11); custom-node (5.6/wamn-bd5) credential
delivery over the HTTP transport + its per-invocation grant registration;
secret rotation/versioning; the 4.3 field masks. The `CredentialRef.kind` field
stays an editor hint (the header name is node config).
