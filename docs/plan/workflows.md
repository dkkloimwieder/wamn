# Workflows

## 1. What a workflow is

See [environment workflows](../architecture/execution.md#environment-workflows) for workflow identity, authoring, enablement, and manual admission.

Vocabulary, one meaning each:

- **workflow**: the definition — graph, trigger, `execution_type`, `execution_role`, `enabled`.
- **run**: one execution of a workflow, a row in `wamn_run.runs`.
- **node**: a compiled wasm component the graph calls — a released operation of the application or a platform node — pinned per environment by digest.
- **route**: a single operation served on the request path. Not a workflow.

Intended uses: reporting (long runs, start cost irrelevant); side effects (send a mail when a record changes); record-state automations (on submit, decide who must approve, validate externally, notify). Performance of this path is not a design driver.

A workflow is not a second application-composition model. The rules that move a record between states are application operations; a workflow coordinates separate operation calls.

## 2. Rulings

| # | Ruling |
| --- | --- |
| R1 — One shape | See [environment workflows](../architecture/execution.md#environment-workflows). |
| R2 — Nodes pinned per environment | `environment.k` pins the exact node versions an environment may run, one per `(name, major)` (R7), in the connector lifecycle. A pin selects nodes for new runs only: a run freezes the exact digest of every node it resolved at admission (`wamn_run.runs.node_digests_json`) and executes those bytes; a re-pin never changes a queued or reclaimed run and is always allowed. The active pin and the retained bytes are different things: a digest's artifact is retained — pulled by digest, as any component is — until no unfinished run names it; retiring (deleting) an artifact is what is refused while such a run exists, never a re-pin. No palette table, no selection row. Admitting a node is an edit and an apply, never a release. |
| R3 — Execution identity | See [environment workflows](../architecture/execution.md#environment-workflows). `invoker` on a manual or attached trigger executes as the calling user with their active role, taken from the authenticated session that made the request — never from a parameter. A manual start is therefore a host route (`workflow.start`, an operation in the application's ACL) admitted by the host against the release it is serving; `wamn-ctl workflow start`, tests and automation all call that route with a session or service-principal token. There is no other start path: the project-admin database start under a supplied service principal is retired with the candidate request. The run row carries the invoker's user id and role, which is new: today `queued_service_caller` accepts only a service principal. `invoker` on an event or schedule trigger: the caller is the system, so the run executes as the `workflow` user with `admin` active, under the same enable rule as `role = admin`; one meaning for the picker — run as whoever caused this. Enable checks two things: the enabler may assign the role (holds it or is admin), and the `workflow` user holds it; `invoker` on an event or schedule is assigning admin and needs admin. Revocation boundary: every node's operation call checks the grant through the host's auth memory as a request would, not through a snapshot taken at claim; a role revoked mid-run refuses the next node under the same semantics a request gets — the revocation drop over NATS reaches every host as it is delivered, and a host that missed the drop is bounded by the memory's TTL. No stronger guarantee is claimed. This changes the router, which today reuses the lease-time grants across nodes. A run records two separate facts: **executed as** (the principal and active role it used) and **invoked by** (what caused it: the session user for manual and attached triggers; the delivery id and, when the triggering change's history row carries an actor, that user, for events; the schedule and due time for schedules), as `invoked_by` and `invoked_by_user_id` on the run row beside the executing principal. `updated_by` on a record is the executing principal: the `workflow` user for type `role` and for callerless `invoker`, the calling user for manual and attached `invoker`; which workflow and which invoker caused the change is one join away through the write log's delivery id. The ACL is unchanged: create, read, update, delete per model plus named operations; a workflow that changes a record runs as a role holding `update` on that model. |
| R4 — Synchronous answer, separate transactions | A request may run several operations and several workflows. A workflow attached to an operation runs after that operation commits, in-process, with a bounded wait; its `respond` terminal is the response. Every operation it calls opens its own transaction; no workflow runs inside a database transaction. On timeout the request answers with the record as it stands and the run finishes on the queue — one owner at a time: the request path releases the run's lease before the queue may lease it, and the queue resumes from logged node outcomes (R5); until the effect writer exists the attached trigger does not ship. The record is the source of truth; a user decision (`approve`) is a user operation that starts the workflow again. No run waits on a user. |
| R5 | Durable execution remains deferred. Run inspection uses `wamn_run.runs` and queue node outcomes. |
| R6 — Events are rows, consumers are per entity | See [environment workflows](../architecture/execution.md#environment-workflows). |
| R7 — Versioning and deprecation | Node versions are semver, and every published digest is a new version. Publish computes the bump the declaration requires and refuses a smaller one: patch = declaration unchanged; minor = additive only (a new optional parameter or port); major = anything else. A workflow names a node by name and major. `environment.k` pins one exact version per `(name, major)`; a patch or minor bump is a re-pin of that slot and every workflow on that major moves with it; a new major is a new slot, coexisting. A major may be marked deprecated: the editor shows it, enabling or editing a workflow that names it warns, unpinning it is refused while an enabled workflow names it. No range is resolved at run time: the environment holds one digest per major and the run pins it. Operations are not versioned; the release is their breaking surface, and `wamn-ctl env apply --dry-run` against a candidate release is the early check, runnable any time, writing nothing. |
| R8 — Workflow identity | See [environment workflows](../architecture/execution.md#environment-workflows). |
| R9 — No workflow in a package | See [environment workflows](../architecture/execution.md#environment-workflows). A package may later carry a workflow template; an instance exists only when an environment asks for one and is the environment's from then on. Epic 4 deletes them when `environment.k` pins replace them. |

See [workflow enablement and authority](../architecture/execution.md#environment-workflows).

Logic inside a workflow — shaping between nodes and conditions — is configuration, never business-authored code. Whether that configuration is an expression language (JSONata is the one node that exists), a library of small typed nodes, or both, is decided with the editor, not here.

## 3. Runtime contracts

See [environment workflows](../architecture/execution.md#environment-workflows) and [event and schedule admission](../architecture/execution.md#workflows).

Do not add `workflow_drafts`, `workflow_versions`, `workflow_admissions`, `workflow_activation`, an activation history, a palette table or a second run store.

## 4. Epics

### Epic 2 — schedules from rows

Run the [wamn-dev cutover](../operations/queued-automation.md#cutover-the-netsuite-pull) and a kind case for the NetSuite pull and the rest_connector item as schedule rows.

### Epic 4 — effect writer and node publication

The effect writer with `output_json`, effect keys, the uncertain rule, resume and retention's authorized deletion (R5); then node publication and `environment.k` pins per R2 and R7, and the attached trigger (R4). Order inside this epic is decided when it is targeted.

### Invariants and checks

| Invariant | Check |
| --- | --- |
| A run executes the bytes it froze | admit a run; re-pin the node to a new minor: allowed; the run executes the frozen digest; a new run executes the new one; retire the old artifact while the run is unfinished: refused |
| A run is exact and durable | queue and reclaim; hash, components, inputs and digest unchanged; a host of another release cannot lease it; a reclaimed run restarts at the first node until the effect writer lands, then resumes at the first node without a success outcome |
| A deprecated major stays until unpinned | mark a major deprecated: enable of a workflow naming it warns and succeeds; unpin it while one is enabled: `env apply` refuses naming the workflow; disable the workflow: unpin succeeds |
| A bump is what the declaration requires | publish a removed port as a minor: refused; as a major: accepted; publish unchanged bytes under an existing version: refused |
| A minor re-pin moves workflows safely | workflow on major 1 at 1.0.1; pin 1.1.0 (new optional parameter); the workflow runs unchanged |

### Later

| Capability | Direction |
| --- | --- |
| Attached trigger (R4) | epic 4, after the effect writer. Trigger row `after = <operation>`, `wait = <bound>`. The entry runs the workflow after the operation commits, in-process, under a run row and a lease; `respond` is the response; on timeout the entry releases the lease and the queue resumes from logged outcomes. The approval and external-validation uses need this. |
| Node publication | epic 4. digest-addressed nodes published without a consuming application, pinned per `(name, major)` to an exact semver in `environment.k`; the wiring node reference gains the node's major (today the join is on the WIT interface version, which every node shares — verify); the semver bump rule enforced at publish from the declaration hash; a host missing an import refuses until the platform version that has it is applied. |
| Editor and run history | `apps/platform_ui`, server-authorized routes, forms from node declarations, run list per workflow, the failed node with its input, retry and cancel. The expression-language-or-nodes choice is made here. |
| Run retention | completed and failed runs are deleted by age, per environment policy (a retention field added to `wamn_run.environment_policies`, the `wamn_run_retention` role); attempt rows are deleted by the same age through the authorized function in R5, since nothing cascades and the trigger refuses a plain delete; failed runs keep longer. Volume is a project's choice: more runs and longer retention cost hardware, not design. Replay-reachability GC waits until something replays. The `condition` on an event trigger is what keeps a hot entity from starting a run per update. |
| Reports, files, mail | workflows over released reads, `http-request`, `http-page`, `file.put`. No platform PDF, template or mail service. |
| Seeding from a package | a package may carry a workflow **template**; an instance is created only when an environment asks (a line in `environment.k`), owned by the environment from then on, never overwritten by apply. R9 stands: a package ships no workflow instance. Until this exists, an integration's operational workflows (the NetSuite pull) are authored in the environment by `wamn-ctl` or the UI. |

## 5. Cost of events from rows

See [event admission](../architecture/execution.md#workflows) for consumers and the materializer.

The materializer can attach to an entity's consumer only while an enabled trigger names that entity.
It can detach when no enabled trigger names the entity.
This option changes attachment only. It never creates or retires a consumer.
Measure the busiest captured entity against the consume-and-filter cost before adding this option.

## 6. Assumptions

| Assumption | Verify | If false |
| --- | --- | --- |
| one row read per host per tick is acceptable | enable a schedule on a running host; it fires at the next due time; the tick's query cost is measured on a host with many schedules | reload on an enable/disable message over event NATS instead |
| an in-process run hands over to the queue on timeout under one lease, resuming from node outcomes | a run that times out at node two releases its lease, is reclaimed, skips node one and completes with no duplicate external effect | a timed-out attached run is failed and the row event starts a fresh one; the request answers with the record |

## 7. Exclusions

No release carries a workflow; no compatibility ranges; no business-authored wasm or SQL; no workload per workflow; no second router, scheduler, queue or identity model; no palette table; no cross-edge transaction; no run that waits on a user; no PDF or mail service; no persisted record without a distinct fact and a reader.
