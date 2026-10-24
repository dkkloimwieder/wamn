# Request execution

A request enters through a declared route and keeps its caller identity throughout execution.
The host binds the route to one component operation or to an admitted wiring in the supplied release.
A route target calls its operation once, with no graph walk and no retry.
For a wiring, the router decides which operation runs next. Native dispatch executes each operation.
A hot HTTP request creates no run or queue row.
Route concurrency limits refuse excess work with HTTP 429.

## Routing and availability

The HTTP ingress path authenticates the caller before invoking application code.
The host compares the route, operation, permissions, wiring, and release facts before dispatch.
The host side of the ingress guest's two imports lives in `wamn-engine`: the [route plugin](../../crates/platform/engine/src/flow_http_routing.rs) and the [delivery plugin](../../crates/platform/engine/src/router_delivery.rs).
`wamn-engine` also owns the generated [binding](../../crates/platform/engine/src/entry_operation.rs) of the guest's `wamn:operation` import, and each host implements it: the cloud over PostgreSQL in `wamn-runtime`, the edge over the SQLite intent store of `wamn-run-state-sqlite`.
A host plugs in through two traits.
`RouteAuthenticator` reads the credential of a protected route, and `RouteDelivery` serves one delivery request.
The cloud implements them as [`PlatformRouteAuthenticator`](../../crates/platform/runtime/src/plugins/route_authentication.rs) and [`RouterDeliveryBridge`](../../crates/execution/host/src/router_delivery.rs).
Publish decides authorization, and the runtime trusts the loaded release.
An application's components compose at build into one component, so a call inside an application reaches no host.
The host begins a transaction for an operation whose admitted operation declares one, whatever SQL it holds. The required permission is the entry's own operation, and nothing derives into it.
The host checks that the original caller holds the entry, once, before the entry runs. A caller holds exactly the permission rows of its roles.
Component membership alone grants no permission to call an operation.

An application host checks the status of its environment for each request, before the auth policy of the route.
It reads its row of `platform.environment` through the guest login of its tenant, as `wamn_app`, and no row means active.
The host keeps the answer in its [auth memory](#auth-memory).
While the status is `inactive`, every route answers 503 with the code `environment-inactive`, and an anonymous route does too.
A failed read answers 503 `authentication-unavailable`.
The control host and an edge box read no status.
The [status routes](#host-run-routes) of the control host write the row.

An attachment with auth policy `none` or `signature` has no principal, so it cannot write.
If the reachable wiring of an anonymous attachment holds a registered operation or a transactional statement, [release publish](../../crates/control/lib/src/publish_release/components.rs) refuses it.

An authored attachment selects signature authentication with `{"modes":["signature"],"header":"X-Signature","credential":"<handle>"}` in its `auth-policy`.
The host resolves the handle under its own project through `WamnCredentials`.
The secret is `{"hmac":{"algorithm":"sha256","secret":"…"}}`.

The ingress reads the body under the route and adapter limits before authentication.
The host computes HMAC-SHA256, a keyed body digest, over these exact bytes.
It compares the digest with the header signature in constant time.
The header value is `sha256=<lowercase hex>`.
A missing, malformed, or incorrect signature or credential returns 401 with an empty body before delivery.

After verification, delivery follows the same JSON mapping and intent rules as a `none` route with the same body.
Signature authentication adds no timestamp or replay window.

The method of a route follows from the kind of its operation, and no author writes it.
Publish writes GET for a route to a `get`, `query` or `projection` operation, and POST for every other route and every wiring.
A GET carries exactly one request item in its query string.
The router reads its bounded body for authentication, but takes the operation input from the query string.
Each top-level member is one parameter, and its value is the compact JSON text of the member.
Parameters stand in byte order of their names, and every byte outside the RFC 3986 unreserved set is escaped.
The router refuses any other spelling, and a request target longer than 8 KiB, with HTTP 400 and the code `invalid-target`.
The [read query codec](../../apps/platform/execution/contract/src/read_query.rs) owns this encoding, and its fixture vectors bind the TypeScript encoder to it.

The host derives the `Cache-Control` value of each read route from its operation kind and auth policy when it serves the route, and the router only writes it.
A `get` sends `no-cache`, and a `query` or `projection` sends `max-age=10, stale-while-revalidate=60`.
A `query` with its feed part (the [change feed](#change-feed)) sends `no-cache`.
Each is `private`, unless the auth policy of the route is `none`, and then it is `public`.
A successful read also sends `Vary: Authorization, Cookie`.
Every other response sends `Cache-Control: no-store`: a write, any status other than 200, and any request that carries `x-wamn-csrf`.
[`read_cache_control`](../../crates/platform/engine/src/flow_http_routing.rs) owns the values.

## Host-run routes

A host-run route is a route that the host answers with a fixed handler instead of a component.
It keeps the route table, the credentials, the CSRF treatment and the operation grant of any route.
The fixed route sets live in [`wamn_catalog::host_route`](../../crates/catalog/model/src/host_route.rs) and are built with the platform.
A serving manifest names the sets it serves in `host-routes`, and it leaves the member out when the list is empty.
The control contract authors its package id `wamn_control`, its version and its route prefixes in [`host_route/wamn.json`](../../crates/catalog/model/src/host_route/wamn.json).
A host route derives its sealed operation id, its path and its attachment id from that file, as generation does for a package operation.
For example, `permission.mine` is `wamn-control:permission/mine@0.9.0`, at `/wamn_control/permission/mine`, with the attachment id `wamn-control-permission-mine-http`.

There are two sets.
Release publish writes the set `application` into every application release.
Its routes admit a PAT or a session and read the project environment database of the release.
The control serving root serves only the set `control`, whose routes admit a browser session only.
Platform builds compose the pinned route guest with an empty private dispatcher for this root.
The route digest pin identifies its source, and the platform HTTP role records its OCI manifest reference.
The control host links `wamn:operation` without binding operation authority, so every operation call refuses.
Control roots have no release record.
Publish refuses an authored attachment under `/wamn_control/`.

[`HostRouteDelivery`](../../crates/execution/host/src/host_route.rs) serves the host routes of the loaded release.
It passes every other delivery to the router delivery bridge.
The caller must match the attachment, and the route checks its operation grant as any route does.
A route of authority `Admin` passes only `admin`.
A route of authority `Granted` passes `admin` and any role that holds the operation of the route as a stored permission row.
`permission.grant` writes such a row, and `permission.list` marks the operation `grantable`.
A host route takes one request item and answers its one outcome, as a generated route does.
A read carries its input as the item, and a write carries `request_id` and its input under `value`.
The answer is `[{"request_id", "value"}]`, or `[{"request_id", "error": {"code", "detail"}}]` for a declared refusal, with no `request_id` for a read.
Any other payload answers 400 `delivery-invalid-payload`.

| Route | Answer |
| --- | --- |
| `wamn-control:permission/mine@0.9.0` | `admin`, and the stable references the caller holds. An `admin` holds every operation the release serves. |
| `wamn-control:user/list@0.9.0` (application) | `users`: each application user with `id`, `email`, `display_name`, `roles` and `admin_covered`, which is true when `project-admin` or `org-admin` covers the user's `admin`. |
| `wamn-control:role/list@0.9.0` | `roles`: every role of the tenant, `admin` included. |
| `wamn-control:role/create@0.9.0` | Takes `role` and creates the empty authored role. Answers `created`. |
| `wamn-control:role/delete@0.9.0` | Takes `role` and deletes the authored role with its assignments and permissions. Answers `deleted`. |
| `wamn-control:permission/list@0.9.0` | Takes `role`. Answers `operations`: each operation the release serves and each stored row of the role, with `served`, `grantable`, `admin_only` and `selected`. |
| `wamn-control:permission/grant@0.9.0` | Takes `role` and `operation`, and writes one permission row. Answers `rows_added`. |
| `wamn-control:permission/revoke@0.9.0` | Takes `role` and `operation`, and removes that permission row. Answers an empty object. |
| `wamn-control:user-role/grant@0.9.0` | Takes `user_id` of an application user and `role`, and gives the role. Answers `granted`. |
| `wamn-control:user-role/revoke@0.9.0` | Takes `user_id` and `role`, and takes the role. An `admin` revoke refuses while the user holds `project-admin` in the project. Answers `revoked`. |
| `wamn-control:number-counter/set@0.9.0` | Authority `Granted`. Takes `application`, `sequence`, `counter`, `key` and the `int64` `value`, and sets that counter. Answers `next_number`. Refusals: `invalid_input`, `sequence_not_found`, `counter_not_found`, `counter_assigned_per_key`, `number_live` and `idempotency_conflict`. |
| `wamn-control:workflow/start@0.9.0` | Authority `Granted`. Takes `workflow_id`, `input_json` and `idempotency_key`, and admits one run of the enabled version of that [environment workflow](#environment-workflows). Answers `run_id`. Refusals: `invalid_input`, `active_role_required`, `workflow_not_enabled`, `workflow_not_served` and `idempotency_conflict`. |
| `wamn-control:control/mine@0.9.0` | `org_admin`, and the projects of the org that the caller administers: every project for an `org-admin`, else the projects where the caller holds `project-admin`. |
| `wamn-control:user/list@0.9.0` | `users`: each member of the org with `principal_id`, `email`, `display_name`, the org-local `status` and `org_admin`. An `org-admin` reads it, and so does a `project-admin` of any project of the org. |
| `wamn-control:user/invite@0.9.0` | Takes `email`, `display_name` and the optional `org_admin`, `project_admins` and `memberships` (`project`, `env`). Answers `principal_id`, `enrolled` and `invited`. |
| `wamn-control:user/activate@0.9.0` | Takes `principal_id` and makes the org membership active. It restores no access. |
| `wamn-control:user/deactivate@0.9.0` | Takes `principal_id`, removes the member's environment memberships, project roles and org roles in the org, then makes the membership inactive. |
| `wamn-control:project/list@0.9.0` | `projects`: projects of the org as `{name, state}`. |
| `wamn-control:org-admin/grant@0.9.0` | Takes `principal_id` of an active member and writes `org-admin`, `project-admin` in every project of the org and a membership in every environment of the org. |
| `wamn-control:org-admin/revoke@0.9.0` | Takes `principal_id` and removes `project-admin` throughout the org, then `org-admin`. Memberships stay. |
| `wamn-control:environment/list@0.9.0` | Takes `project`. Answers `environments`: environments of the project as `{name, state}`. |
| `wamn-control:environment/status@0.9.0` | Answers the digest, rollout, workload conditions, and live set of one environment. Adds nullable counts `users_with_role`, `roles`, and `memberships`. |
| `wamn-control:project/create@0.9.0` | Creates a project with no environment. |
| `wamn-control:release/profile@0.9.0` | Takes `project`, `env` and the `digest` of a release installed in that environment. Answers `operations`: for each export of the release's components, the members it `reaches`, each with the exports it holds, `imprecise` when it is a base member's whole statement set, and per statement the tables it reads, writes, deletes and locks, derived from the release's SQL over the environment's table columns, and `roles`: each role of the environment with the `operations` it holds, `admin` every operation of the release. Writes nothing. |
| `wamn-control:member/list@0.9.0` | Takes `project` and an optional `environment`. Answers `members`: each user with a membership or `project-admin` in the project, with `principal_id`, `email`, `display_name`, `org_admin`, `project_admin` and `environments`. |
| `wamn-control:member/grant@0.9.0` | Takes `project`, `env` and `principal_id` of an active member, and writes the membership of that environment. |
| `wamn-control:member/revoke@0.9.0` | Takes `project`, `env` and `principal_id`, and removes the membership of that environment. It refuses while `org-admin` or `project-admin` covers the environment. |
| `wamn-control:project-admin/grant@0.9.0` | Takes `project` and `principal_id` of an active member, and writes `project-admin` and a membership in every environment of the project. |
| `wamn-control:project-admin/revoke@0.9.0` | Takes `project` and `principal_id`, and removes `project-admin` in the project. It refuses while the user holds `org-admin`. Memberships stay. |
| `wamn-control:environment/activate@0.9.0` | Takes `project` and `env`, and makes the environment active. Answers `project`, `env` and `status`. |
| `wamn-control:environment/inactivate@0.9.0` | Takes `project` and `env`, and makes the environment inactive. Answers `project`, `env` and `status`. |
| `wamn-control:project/activate@0.9.0` | Takes `project`, and makes every environment of the project active. Answers `project` and `status`. |
| `wamn-control:project/inactivate@0.9.0` | Takes `project`, and makes every environment of the project inactive. Answers `project` and `status`. |

`environment.status` also answers three nullable `int32` members.
`users_with_role` counts human users with at least one role, once per user.
`roles` counts all roles in the environment.
`memberships` counts its rows in `identity.project_env_memberships` on the control connection.

The first two counts share one query through the existing environment administration login, within a read-only transaction.
Each count read has a two-second timeout, and both reads run concurrently.
A failed or timed-out read logs a warning and returns null for its counts, without failing the runtime status answer.

The control host selects one status source. Kubernetes reads the release namespace.
With `--local-control`, the local source reads the development loop's endpoint record and queries its selected host through NATS.
Both sources return the same digest, rollout, workloads, and live set.

An application route runs under the administration credential, in a host-owned READ COMMITTED transaction that binds the caller and the sealed operation id.
A host-run write stamps that sealed operation id in its history entries.
A role or permission write first takes the tenant lock that `wamn-ctl` and release reconciliation take.
The writes are the functions of [`wamn_platform_identity::application`](../../crates/identity/platform/src/application.rs), which the role verbs of `wamn-ctl` also call.
`user_role.revoke` reads `project-admin` through the identity reader of the host, because `org-admin` writes `project-admin` in every project of the org.
`permission.grant` writes one permission row. A row the serving release does not serve grants nothing.
An application refusal is a declared error of its route, with `field` in its detail: `admin_fixed`, `invalid_input`, `role_not_found`, `operation_not_served`, `operation_not_grantable`, `permission_not_held`, `user_not_found` or `admin_covered`.
The contracts of the application routes live in [`host_route/contracts`](../../crates/catalog/model/src/host_route/contracts), and the generator projects them into the TypeScript client `@wamn/control-client` in `apps/target/wamn/wamn_control/client-ts`, which Git does not hold ([building](../operations/building.md#host-route-clients)).
The administration screens in the [platform UI](../../apps/platform_ui/README.md) call these routes through that client and write no database directly.
The role screen reads operations from `permission.list`, and the user screen reads roles from `user.list` and permissions from `permission.list`.
The control lists `project.list`, `user.list`, `environment.list`, `member.list`, and `control.mine` accept optional `limit` and `cursor` members.
Either member starts paging. `limit` accepts integers from 1 through 100 and defaults to 100 in paging mode.
Explicit nulls refuse. A request with neither member returns every row.
Each answer adds nullable `next_cursor` and `total` members. `control.mine` repeats `org_admin` on every page.
`total` is the full count only when a first page exhausts the set, or a whole-result request reads everything.
Later pages return null totals, even on the last page. No list runs a count query.
`member.list` also accepts an optional `environment` member. With it, the answer keeps only the users with a membership in that environment of the project, and each row's `environments` names only that environment.
The query applies the filter before the keyset and the limit. An unknown environment returns an empty list. An explicit null or a blank name refuses with `invalid_input`, `detail.field` `environment`.

A cursor is an opaque token for the next page. Each query resumes after its last key and reads at most `limit + 1` rows.
Projects sort by project identifier. Environments sort by environment name within the project.
Users and members sort by email, then principal UUID. SQL comparisons use the same database collation as the order.
Cursors encode canonical compact JSON as unpadded base64url and bind the operation, authenticated principal, org, applicable project, the `member.list` environment or its absence, and order.
Malformed or mismatched cursors return `invalid_input` with `detail.field` set to `cursor`. Bad limits name `limit` in that field.
Every page repeats authorization. Pages reflect current data, so an email change can move a user between pages.
The contracts retain version 0.9.0. `environment.status` and `release.profile` retain their whole-result contracts.

`control.mine` reads `wamn_system` through the `control` login of the org.

Every other org route needs a current `org-admin` row of the caller in the token's org, and answers 403 `permission-denied` without it.
A project route needs that row, or a current `project-admin` row of the caller in the project that the request names.
A write runs in one transaction on a second `control` login, which holds one write at a time.
The transaction binds `app.user_id` to the caller and `app.operation` to the sealed operation id, and it checks the role again.
The writes are the functions of [`wamn_platform_identity::org`](../../crates/identity/platform/src/org.rs), which `wamn-ctl invite` and `provision-org --admin` also call.
A refusal is a declared error of its route, with the input member it names in `field`: `invalid_input`, `project_not_found`, `environment_not_found`, `user_not_active`, `user_not_found`, `admin_covered` or `user_refused`.
A write that changes administrative authority or membership also changes the application rows of each environment it covers, one transaction per environment, with the environment's `wamn_administration` login.
The control host reads that login from the mounted Secret `wamn-control-administration-<org>`, one file per `<project>--<env>`, at each write (`--control-administration-dir`, `WAMN_CONTROL_ADMINISTRATION_DIR`).
A grant commits its system rows first, and then writes the `user` row, with `admin` for `org-admin` and `project-admin`.
A revoke or a deactivation removes the application rows first, and commits its system rows only when every environment is done.
If an environment fails, the route answers `application_write_incomplete` with that `environment` and the environments that completed, and the system rows of a revoke stay as they were.
The four status routes need `org-admin`, and they write `registry.project_envs.status` through the `control` family, which holds `UPDATE` on that column only.
Each status route also writes the status into the one row of `platform.environment` in each environment, with the same administration login.
An inactivation writes the environments first and commits its system rows last, and an activation commits its system rows first.
Data, membership and role rows do not change with the status, and CDC stays attached.
`user.invite` calls identity `POST /users` and `POST /invitations` with the operator certificate of `--pat-issuer`, `--pat-client-cert`, `--pat-client-key` and `--pat-server-ca`, through [`wamn-identity-client`](../../crates/identity/client/src/lib.rs).
It writes the membership and the grants between the two calls, and it calls `/invitations` only when the user has no password.
The host refuses an email or a display name that the identity rules refuse before it calls identity, and answers `user_refused` when identity refuses the user.
The contracts of the control routes live in [`host_route/control/contracts`](../../crates/catalog/model/src/host_route/control/contracts), and the generator projects them into the TypeScript client `@wamn/control-org-client` in `apps/target/wamn/wamn_control/control/client-ts`.
The control set has its own client because both sets serve `user.list`.

The control serving root has no package, component, route, database or guest connection.
`wamn-host --control` serves it for one org.
A control host holds no project credential and no application release, and an application host never holds the `control` login.

A read also carries an ETag, which the host derives and compares.
A `get` has a strong ETag from the manifest digest of the release and the revision field of the record that it returns.
The generator writes the revision column of the model into each `get` contract. The model's operations name that one column as `revision_field`.
A `get` of a model with no revision column has no strong ETag, and the generated client index names that model.
A `query` or `projection` has a weak ETag from the manifest digest, declared relation names, and the current WAL position.
A `query` with its feed part has a weak ETag from the manifest digest and the `next_cursor` it returns. It reads no WAL position, and it runs before the host compares the tag.
The [model version source](data-access.md#model-versions) covers commits from every host and can also change after unrelated writes.
Publish writes the relations and the revision field into the serving manifest route from the generated contract, and a history table stands for its model relation.
The router forwards `If-None-Match` on a GET, and the host compares it with the weak comparison.
For a list, the host applies the operation grant and reads the WAL position.
On a matching ETag, it answers not-modified without running the read.
For a `get`, the host runs the read and then compares.
On not-modified, the router answers 304 with the ETag, the `Cache-Control` and `Vary` of the route, and no body.
A read without a revision field, or with no declared relations, has no ETag, and a failed version read leaves the list without one.
A `get` tag follows `row_version`, so a row that is deleted and created again under the same natural key can match an old tag.
[`read_cache`](../../crates/execution/host/src/read_cache.rs) owns the tags.

The entry-only `wamn:operation/operation@0.1.0` interface also provides `wal-position()`.
It reads the WAL position on the open operation's connection and refuses `not-begun` outside an operation.
The [operation boundary](data-access.md#the-write-log) owns its transaction lifetime.
The composed entry computes ETags and handles conditional reads for base routes.
The delivery bridge retains this behavior for wiring attachments.
The routing provider also projects the ETag inputs from the loaded release.
Those inputs declare the release digest and either the result's revision field or the sorted relations for a list.
A get without a revision field and a list without relations carry no ETag inputs.
The guest receives the kind and fields as data, without application-specific names in the route code.

The entry holds the platform ceiling of 50,000 rows for one base read.
It refuses a read whose limit is above the ceiling before it authorizes the operation.
Host routes and wiring attachments carry no entry ceiling.
`route-definition.read-limits` carries page bounds and required band starts from the released input schema.
The entry enforces those bounds and keeps the existing HTTP 400 pointer response.

`route-definition.operation-deadline-ms` carries the host operation limit.
The entry measures one shared budget with its monotonic clock.
An unreached item receives `timeout` with the message `not reached before deadline` and claims no key.
The host epoch deadline remains the hard execution bound.

Authentication retains incoming trace context in the HTTP Store, including anonymous requests.
PostgreSQL callbacks restore that context when they create spans.
The host records operation delivery counters at begin, commit, and rollback.
A dropped open operation records an execution failure and releases its authority.

A `query` replies with one page: the outcome list of one item, with the rows under `item` and the cursor under `next_cursor`.
The entry writes the page as one JSON body.
The entry bounds a page by the declared contract maximum, usually 100, and by the platform ceiling.
The generated application codec validates the remaining input.
The memory of a read is bounded by the statement's declared limit and the platform ceiling.
The test `a_read_of_100000_rows_refuses_at_the_ceiling` in [`route_interface_live`](../../tests/integration/src/route_interface_live.rs) shows the refusal.
The composed entry calls the application query through its private dispatch interface, as it calls a projection.
The query returns its page and the WAL position of a list ETag from one `read-json` call, and it reads through `statements.run`.
Host routes and wiring attachments retain [`wamn:router-delivery` 0.4.0](../../crates/execution/host/wit/deps/wamn-router-delivery-0.4/package.wit).

`invoke_operation` takes an optional intent context, and with one a write logs one intent for each input item and runs only the new items.
The router driver and the edge pass that context for each write operation.
Composed base routes claim through `begin-operation`, with the same item keys and canonical request hashes.
In the cloud the store is the [write log](data-access.md#the-write-log).
On the edge it is the SQLite intent store of `wamn-run-state-sqlite`, one file with one writer.
Its `intents` table holds one row for each operation call, unique by tenant and idempotency key.
A begin with a known key and another input hash is a conflict.
A begin with a finished key answers the stored outcome, and an open intent with no outcome is uncertain until an operator resolves it.
A retry that the write log answers commits nothing, so change capture publishes no event for it and no post-commit work runs a second time.
Publish writes two input facts into the serving manifest route from the generated contracts: the key field (`idempotency`) and the canonical item form (`canonicalization`).

A host or wiring route input that fails its schema returns HTTP 400 with the code `schema-invalid`.
Base routes keep this response for declared page and band limits.
Their generated application codecs refuse other invalid input through the operation contract.
The body carries the RFC 6901 pointer of the offending value in `data.pointer`:

```json
{"error":{"code":"schema-invalid","data":{"pointer":"/0/change/created_by"}}}
```

For an unexpected property, the pointer names that property.
An unparseable payload carries the root pointer `""`.
A platform cause, such as a missing or uncompiled schema, gives a body with no `data`.
The [route validator](../../crates/platform/engine/src/flow_http_routing.rs) and the [HTTP route adapter](../../apps/platform/ingress/http-route/src/lib.rs) own this response.

An unknown hostname returns the native 404 response.
An explicit hostname in the release returns 503 while its route binding is absent.
The adapter adds no `Retry-After` header and leaves application responses unchanged.
A selected route whose native dispatch handle disappears still returns 404.
Wildcard expansion, operator aliases, and readiness based on route bindings remain absent.

The [expected-host adapter](../../crates/platform/engine/src/expected_router.rs) owns this narrow response distinction.
Service readiness does not establish route-aware traffic removal.
The native route controller can reach Pods directly, so a readiness change alone does not remove every rebind outage.

## Native dispatch

One native application owns a loaded release or frozen candidate.
Native dispatch reuses compiled component bytes. Untrusted components receive a fresh store, which holds guest memory and resources, for each call.
The deployment owner can select reviewed component digests for native warm-instance reuse.
[Deployment configuration](../operations/deployment.md#trusted-component-reuse) owns this selection and pool sizing.
Application manifests cannot grant that trust.

An application is one composed component, and it loads as its own native workload.
Publication composes the pinned HTTP route guest with the application and records the entry reference in serving manifest format 11.
A deployed entry is recorded as `entries@sha256:<manifest-digest>`, a path relative to the component artifact base.
The host joins that path with its `WAMN_COMPONENT_ARTIFACT_BASE`, as it does for the members.
The host records the joined reference by workload id before it calls the runtime's default start handler.
The runtime pulls, verifies, and compiles the component. The operation binding checks that the recorded reference is the release's entry path under a valid base.
The composed entry uses fresh Stores and calls base operations inside the component.
The release chart fixes its `poolSize` at zero.
Its image is the host group's `WAMN_COMPONENT_ARTIFACT_BASE` joined with the release entry path.
Receiving carries no trusted warm digest or native pool override.
Host routes and wiring attachments keep the router-delivery import.
Local publication records an absolute `file:` reference to a digest-named artifact. The existing local file loader checks its bytes before binding.

No two applications share a store, and wash-runtime links no application to another.
Applications in one release can use different lifetimes.
A call across applications is a workflow call through the host.

Each call receives a new host-owned authority scope.
Capability calls use that scope, and cancellation revokes it independently of native store teardown.
A retained guest context cannot select the next call's permissions.
A call that leaves host resource handles open discards those resources and retires its instance while preserving its completed result.
The host never clears a resource table and then reuses that instance, because old handles can alias new resources.

Trusted component code owns request-local caller data, caller-dependent cache cleanup, and completion or cancellation of spawned tasks before return.
The host does not sanitize guest memory or enforce those promises against faulty or malicious trusted code.
Use fresh instances wherever that trust is unacceptable.
`maxConcurrency = 1` serializes calls on an instance. It does not isolate guest memory or background tasks.
One absolute deadline covers initialization and execution.
A guest cannot extend that deadline.

Connection activation changes the generation selected for new work.
Admitted runs that carry a generation pin continue to use that exact immutable generation.
A disabled instance or an unavailable pinned credential refuses execution without selecting a newer generation.
Superseded generation records remain immutable. Retaining a record does not extend the lifetime of its credential.


The host bounds each requested node deadline to 1–30,000 milliseconds and continues execution.
When the host changes a deadline, the delivery report carries the node, `requested-ms`, and `effective-ms`.
HTTP responses carry this JSON array in the `wamn-deadline-adjustments` header, including execution failures.
Application response bodies remain unchanged.
Queued runs store the array in `runs.deadline_adjustments_json` before settlement or retry, under the current lease.
The column retains the latest attempt that reports a change.
An absent header or null column means that no change was reported.
Nested calls still inherit the enclosing deadline. Their remaining time is not a separate node deadline adjustment.

Native dispatch retires warm instances after traps, deadlines, or cancellation.
Caller authority ends immediately. Physical teardown follows native cancellation rules. The engine sets wash-runtime's abandoned-call grace to zero, so a cancelled call's store traps at its next epoch yield and returns its memory.
Successful calls can reuse an eligible instance only when no host resource handles remain.
A full pool serves overflow from fresh stores.
Native idle reclamation and a 1,000-call instance limit bound retention.
Pools belong to one immutable release or candidate and never cross tenant or environment boundaries.
A replacement release or deployment trust configuration uses new pools.
Credential checks retain the exact pinned generation rules above.

Pool capacity does not limit request admission or total memory.
A chart host derives its guest memory budget from its cgroup: three quarters of the 768Mi limit, which is 576 MiB.
A process memory guard at 90 percent of the limit refuses guest growth and new workloads in every mode.
The guard samples resident memory once per second, so it is not a hard allocation cap.
The host runs guest memory and raw-socket egress in count mode ([native alignment](native-alignment.md#retained-wamn-implementations)).
Count records growth past the budget and connections that the socket policy refuses, and allows them.
The host flags `--guest-memory-mode` and `--socket-egress` select enforce.
Each linear memory keeps its 256 MiB ceiling.
Existing route and queue admission limits and deployment memory limits remain independent.
Wasmtime's pooling allocator remains enabled under its separate resource limits.
The release chart sets how many bytes each allocator slot keeps resident after use.
Dev activation and the local benchmark read the same values ([host environment](../operations/development-loop.md)).
These values change residency only. They do not increase slot counts or memory ceilings.
Wasmtime leaves async stack zeroing disabled, so its async stack residency value has no effect under this configuration.
Native epochs bound guest execution. Duration metering does not grant instruction fuel.
All cleanup paths retain their resource bounds and return failures to their owner.

The [engine operation module](../../crates/platform/engine/src/operation.rs) runs one component export through native dispatch with `invoke_operation`.
It owns the deadline, workload loading, and invocation state.
It reaches the host through two traits: `ApplicationHost` gives the loaded application, and `InvocationPolicy` grants and revokes the authority of each call.
The [host operation module](../../crates/execution/host/src/operation.rs) implements both traits and owns authority.
The [driver](../../crates/execution/workflow/src/router_driver.rs) in `wamn-workflow` walks a wiring graph and calls the operation module for each node.
The [route path](../../crates/execution/host/src/route.rs) calls it once for a route target and never enters the driver.
A route reports a retryable or rate-limited error as the failure that a wiring reports after its last attempt.
The caller decides whether to send a new request.

Typed operation inputs and successful results contain owned lists or records.
Admission checks the context, node errors, and every nested value.
It refuses store-owned resources at these boundaries.
A composed call inside an application uses the typed entrypoint.
JSON adapters serve HTTP and dynamic routing.
The pure router, `wamn-router`, owns deterministic graph decisions.
After initialization, the host records caller, SQL, claims, causation, and effect authority in an invocation scope.
Native callbacks restore the trace context from that scope.
Cleanup revokes the scope and clears bindings after success, failure, cancellation, or owner shutdown.
The [queue](../../crates/execution/workflow/src/queue.rs) in `wamn-workflow` owns durable queue claims and settlement through the `RunStore` trait of `wamn-run-state`.
The queue owner binds these transactions to `wamn_run`, independently of the application data schema and database default search path.
HTTP admission and queue delivery use separate concurrency bounds, so one workload cannot consume the other's capacity.
Each host replica polls the durable queue and claims work through database leases. Replicas need no wake service or process-local handoff.

The host opens each claims transaction with `BEGIN ISOLATION LEVEL READ COMMITTED`, so a session default cannot change the level that the [write log](data-access.md#the-write-log) retry depends on.
The guest transaction of a read (get, query or projection) opens with `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY`, so its statements read one snapshot.
For a new logged update, the host sends `BEGIN`, local claims, and the write-log claim together, then waits before it runs the guest.
A duplicate reads the stored claim and never runs the guest.
The guest waits for its SQL result before it returns the value that the write log stores.
The host then sends the write-log finish, the transactional causation message when present, and `COMMIT` together.
A missing finish row causes a SQL error, so the queued commit cannot preserve an unfinished write.
With one guest statement and warm prepared statements, the update transaction takes three round-trips and seven top-level SQL statements with causation.
Without causation it takes six statements. Preparation on a cold connection and duplicate-claim reads add trips.
The guest return value prevents the guest statement and finish from sharing one trip.
No counter trigger, changed-relation read, or counter reset runs on the write path.

The host binds the executing principal as `app.user_id` in each claims transaction that it opens.
[Record history](data-access.md#record-history) stamps that principal on every write.

- An authenticated caller of a route binds its principal id.
- A post-commit registration delivery binds `wamn:materializer`.
- An automation delivery binds its admitted service principal.
- A schedule delivery binds the service principal of its application.
- A workflow run binds its execution principal.
- A legacy queue delivery or management candidate case binds `wamn:executor`.

The same transaction binds the executing operation as `app.operation`.
A node binds the operation token of the entry that it runs. A composed callee runs under that token.
A registration delivery binds the token of its handler.
A host queue claim, reap, renew, or complete transaction binds the `wamn:executor` credential-class identity.
An absent operation binds an empty value, so a pooled connection keeps no earlier operation.
The [record history log](data-access.md#history-tables-and-the-log-trigger) records the operation in each entry.

The same transaction binds the composing application that runs the operation as `app.application`.
One application is one composed workload, so the value is the package id of the composed entry's composing member.
A base operation reached through the composed entry binds the composing application, not the base package.
A component outside the composed entry binds its own package id.
A host queue transaction, a host route transaction, and a platform read bind no application.
An absent application binds an empty value, so a pooled connection keeps no earlier application.
The claim is for logs only. [Numbering](data-access.md#numbering) takes the application from host state, not from this claim.

A platform claims transaction that only reads binds no principal and no operation.
A bound principal alone does not move a read out of the autocommit path.

For that path, each pooled connection remembers its last schema and runner.
If these values match, the read executes only its application statement.
If either value changes, the host pipelines the session configuration before the read.
An absent schema restores the server default, and an absent runner clears the prior runner.
This reuse stores no principal, role, operation, or application claim. Those claims stay local to each transaction.

The [claims owner](../../crates/platform/runtime/src/plugins/wamn_postgres/claims.rs) refuses a transactional statement with no executing principal before it reaches PostgreSQL.
That refusal carries SQLSTATE `55000` and the message `actor-required`, as the stamp trigger does.

## Workflows

A workflow is a released wiring that runs off the request path.
The [workflow contract](../../crates/execution/workflow/src/contract.rs) in `wamn-workflow` is the trait `Workflows`, with four operations.

- `start` admits one released wiring under a service principal. It writes the run and queue row together. A repeated key returns the first run.
- `park` moves the `available_at` of a queued run to `infinity`, so no claim takes it. The run keeps the status `dispatched`, and a running or finished run refuses.
- `release` returns a parked run to the queue.
- `list` returns the runs of one environment, newest first, with their status and their queued and parked flags.

`PostgresWorkflows` admits automation with project-admin authority.
The `wamn-ctl workflow park`, `release` and `runs` commands call it ([queued automation](../operations/queued-automation.md)).
Its SQL lives in `wamn-run-state` beside the claim SQL.

An application event starts a workflow that the package declares in `wamn.json` under `workflows`.
The declaration names the wiring and a registration: the source package, the entity, and the row operations.
The registration can add a JMESPath `condition` over the event, and the materializer delivers only the events for which it holds.
Apply-package writes the registration row under the workflow id, and publish derives a `ServingRegistration` for the named wiring.
The wiring can enter at any node, while an event handler's wiring enters at the handler.
The materializer delivers the event, and the host admits it as a queued run under the authority of the release that declares the workflow.
The executor credential inserts only the event run columns, and the delivery id is the idempotency key, so a redelivered event admits one run.
The queue runs the wiring with no caller, and an event handler's wiring still walks when its event arrives.
A node that needs an operation grant refuses, so an event workflow runs palette nodes.
`list` shows an event run with the trigger `event`, and `park` and `release` act on it.

A package declares no schedule (docs/plan/workflows.md R9).
A schedule workflow is a row of `catalog.workflows` with trigger `schedule` and a five-field cron in UTC, parsed by `croner`.
A release published before this change may carry `workflow.schedules` in its manifest; the host reads it with the field ignored.

Every application host runs one schedule task, which wakes at each minute boundary.
Its first tick is strictly after startup.
At each tick it reads the enabled schedule rows of its application with one `SELECT`, and calls `WamnPostgres::admit_schedule_run` for each row whose cron names that minute.
An enable or a disable between ticks takes effect at the next tick.
The run has input `{"due":"<due RFC 3339>"}` and key `schedule:<application>/<workflow id>:<due RFC 3339>`, with UTC spelled as `Z`.
Replicas use the same key, so concurrent admissions return one run.
The run executes as the `workflow` user with its execution role, or with `admin` for type `invoker`.

Apply creates one service principal per application that composes `wamn_integration`, in each environment.
Its subject is `wamn-app-<org>--<project>--<env>--<package id>`.
The tenant service row uses `<subject>@<platform-domain>` as its email.
Repeated applies retain its identity and grants.

Publish authorizes the operations that the application declares as dependencies.
Apply assigns a role with those permissions. Repeated applies write no principal or role rows.
The executor's database role reads only `id`, `type`, `status`, and `email` from `platform."user"`.
It can insert the principal into a run but cannot change that identity afterward.

The queue reads its current permissions, and the router authorizes each registered consumer command through the existing operation check.
A missing permission refuses that command.

Missed or refused ticks produce a log entry without catch-up, and host shutdown cancels the task.

The [JSONata node](../../apps/platform/execution/jsonata/src/lib.rs) is a palette node compiled to wasm.
Its parameter `expression` is JSONata text, and the node evaluates it on its input inside the guest.
The result leaves on port `main`, or on port `items`, whose schema is the item array that `label-render` takes.
The node is a std guest: it imports `wasi:random` at the version that Rust std links, and the virtualizer passes that import through.

The WMS workflow `movement_label` is the first example.
A `packaging` update starts it when the registration condition `old.location_id != new.location_id` holds, so only a move starts it.
It stores one packaging label for each move, keyed by `{packaging_id}/{row_version}`, at the object path `wms/{key}`.

### Environment workflows

An environment workflow is a wiring that an operator authors in one environment instead of in a package.
Its trigger is `manual`.
Each version is one row of `catalog.workflows` in the project database.
The row holds the application, the workflow id, the version, the graph, the wiring hash, the execution type, the execution role and `enabled`.

`wamn-ctl workflow author` gates the graph against the release that the environment serves, and stores the version disabled.
The same bytes under the same version converge. Other bytes under that version refuse.
A stored version never changes except `enabled`, and at most one version of a workflow is enabled.

`enable` and `disable` take the lifecycle lock of the coordinate, as `env apply` does.
While another verb holds the lock, they refuse with `apply in progress`.
`enable` gates the graph again, then disables the enabled version and enables this one in one transaction.
`disable` sets only `enabled` to false. Queued and running runs finish under the version they pinned.

The execution type decides who a run executes as.
A `role` workflow executes as the `workflow` user with its execution role.
The enabler must hold that role or `admin`, and the `workflow` user must hold that role.
An `invoker` workflow executes as the caller with the active role of the caller.
Every environment holds one `service` user named `workflow`, and the platform grants it no role.

The route `workflow.start` admits a run.
It reads the enabled version and gates the graph against the release that the host serves.
It refuses `workflow_not_served` when the graph does not gate or its hash differs from the stored hash.
The active role of the caller is `admin` for a holder of `admin`, else the one role that the caller holds.
Any other caller refuses `active_role_required`.
The run records the release, the wiring hash, the component digest of each node, the principal and role it executes as, and the caller as its invoker.
The idempotency key is scoped to the caller and the workflow.
A repeated start with the same key and input answers the first run, and another input refuses `idempotency_conflict`.
The run and its queue row commit in one transaction.

When the queue claims the run, `queued_run_caller` resolves its principal.
The principal must be an active `service` or `user` that holds the run's role.
The caller holds the permissions of that role only, and holds `admin` only when the role is `admin`.
The host reads these grants again before each node, so a role revoked during a run refuses the next node.
A run with no role is an automation or schedule run.
Its principal must be an active `service`, and it holds the permissions of every role of the principal.

## Results and effects

A wiring edge carries the declared route envelope.
Items contain either `value` or `error`.
A write item retains its `request_id`. A read item carries none, and its outcomes match its items by position.
Each node consumes that envelope and emits its declared output port.
Connected ports require identical JSON Schemas.
A target with multiple input ports requires an explicit `to_port`.
A hop limit bounds graph traversal.
The router does not invent item fan-out or split one emission into independent branches.

A completed database mutation and a later failed effect remain distinct outcomes.
A response can establish partial completion only when its declared contract carries the committed result and failed outcome.
A transport failure or an unknown response does not establish that nothing ran.
Automatic whole-command replay cannot follow from an inner operation's idempotency.

The blob writer stores each successful item and preserves existing error items.
It enriches the value with storage information instead of replacing the original result.
A write failure fails the emission as a whole.

Commands use the [transaction rules](data-access.md#inputs-queries-and-transactions).
Effects use the [capability contracts](capabilities.md), including their exact refusal and outcome types.
No second error vocabulary replaces a public WIT or serialized application contract.

## Caller authentication

The route declares PAT authentication, session authentication, or both.
Receiving and Acme publish both modes on their existing authenticated application routes.
Both modes use the same operation permissions. Private event handlers remain private.
A personal access token, or PAT, is an opaque bearer credential.
PAT authentication reads the current principal, credential, organization scope, and applicable membership before admitting the request.
Authorization reads the operation permissions of the principal.
The host keeps both answers in its [auth memory](#auth-memory).
Service credentials retain their declared scope and cannot mint a user session.

A session token carries the user roles selected at minting and its revocable authority.
For each new request, the host checks the principal, environment membership, and password login or source PAT.
It intersects signed roles with the held roles and their permissions for the active tenant user.
A caller that holds `admin` holds every operation of the serving release. Any other role holds its stored stable references `<package>:<interface>/<operation>`, and the host matches each sealed operation of the release by its reference.
Missing, revoked, expired, or mismatched authority returns 401.
An unavailable identity read refuses admission with 503. The read has a five-second timeout.
Logout, reset, account disablement, membership removal, and PAT revocation reach an admitted PAT or session with the next drop of the [auth memory](#auth-memory).
A host that cannot receive drops caches nothing, so it reads every request.
The TTL of 60 seconds is the bound only if a drop is lost.
Already admitted work retains its caller. Revocation does not cancel that work or undo its effects.

### Auth memory

The host answers the auth facts of a repeated caller from its memory, so they cost no Postgres round-trip.
The [memory](../../crates/platform/runtime/src/auth_cache.rs) is the local replica of the NATS KV bucket `wamn-auth` on the scheduler NATS.
The key `<tenant>.<principal>` holds the grants of a principal and whether the tenant has its user row.
The key `<tenant>.environment` holds the environment status.
One read of `platform."user"`, `platform.user_role` and `platform.permission` answers the permission check and the user-row check of the invocation bind.
A host that reads a fact puts it with the revision of the key from before the read, so a delete during the read refuses the put.
Each host watches the keys of its tenant and keeps each value it sees, so a fact one host reads serves every host of the tenant.
The control host serves every tenant, so it watches every key.
The bucket keeps a value for at most 60 seconds, and so does the replica.

An admitted PAT and an active session stay in the host that read them, because a token answer never leaves the host.
The host keeps only a positive identity answer, so a new PAT or a new login works on its first request.
A delete of the key `=identity.<principal>` drops the PATs and sessions of the principal on every host.
A tenant name holds only `[A-Za-z0-9_-]`, so that key never meets a tenant key.
A host does not keep a token answer when any identity key changed during its read.
While the watch is broken, the host keeps no fact and opens the bucket again.
A host without the bucket keeps no fact either, and reads every request from Postgres.

Every function that writes one of these facts takes an [`AuthDrops`](../../crates/identity/platform/src/auth_changes.rs) and records the change in it.
Its caller deletes the keys of the list after the commit.
[`auth_drops_live`](../../crates/identity/platform/tests/auth_drops_live.rs) runs each writer once and checks the drop it records.
A change of a user row or a user role deletes the key of the principal.
A change of a role, a permission or the environment status deletes every key of the tenant.
A PAT revoke, a logout, a reset, a replayed renewal, a membership revoke and a disabled principal delete the identity key of the principal.
The writers are the application host, the control host, the identity service and `wamn-ctl`.
The identity service and `wamn-ctl` read the connection from `WAMN_AUTH_NATS_URL`, `WAMN_AUTH_NATS_TLS_CA`, `WAMN_AUTH_NATS_TLS_CERT` and `WAMN_AUTH_NATS_TLS_KEY`, with a client certificate of the wasmCloud CA.
`WAMN_AUTH_NATS_TLS_SERVER_NAME` names the server certificate when a port-forward reaches the server.
A lost delete waits for the 60 seconds.

The exact audience is `urn:wamn:project-env:{org}:{project}:{env}:{instance_suffix}`.
The configured environment supplies its current suffix.
Identity offers and mints no audience of an environment whose `registry.project_envs.status` is `inactive`, so the shell does not list it.

Session tokens use Ed25519 with `typ=wamn-session+jwt`.
They require `iss`, `sub`, `org`, `aud`, `roles`, `exp`, `iat`, `jti`, and `authority`.
`authority` is exactly one object: `{"login":"<login UUID>"}` or `{"pat":"<source PAT UUID>"}`.
The optional `csrf` claim is the lowercase SHA-256 hex of a CSRF token. Only the cookie carrier sets it.
The password issuer binds the existing login record. PAT exchange binds the authenticated source PAT.
The `kid` selects a key only within the configured issuer.
Wrong issuer, organization, audience, algorithm, signature, or time bounds returns 401.
Unknown roles grant no implicit authority, and an empty permission set returns 403.

A browser presents the session token in the `__Host-wamn-session` cookie instead of the `Authorization` header.
The host reads that cookie only when the request has no `Authorization` header and the route admits a session.
A request that carries both refuses with 401, and so does a repeated session cookie.
The host verifies a cookie token as it verifies a bearer token, and then applies the CSRF check.
A cookie token without the `csrf` claim refuses with 401.
The CSRF check needs one `x-wamn-csrf` header whose SHA-256 hex equals the claim.
A read route (kind `get`, `query` or `projection`) needs no header, because a read changes nothing.
Every other route and every attachment to a wiring requires the header. The cookie carries the `csrf` claim on every route.

The token lifetime is at most 900 seconds, with 30 seconds of time tolerance.
Minting anchors expiry to the start of credential validation and records the actual signing time in `iat`.
A delayed mint cannot restart the lifetime.
The host reconsiders admission freshness after permission reads.
Age bounds limit delayed admission without promising that identity remains unchanged during a pause.

Authentication deadlines bound new admission and do not cancel work already accepted.

The [session token owner](../../crates/identity/session/src/token.rs) defines the token and time rules.
The [session verifier](../../crates/identity/session/src/verifier.rs) takes its keys from a key source: the issuer over HTTPS in the cloud, or a key file on the edge.
PAT exchange keeps no login record or renewal credential.
The browser client holds no token. `@wamn/web-runtime` signs in with the cookie carrier and renews on load and before expiry.
Password sessions have no PAT fallback. Renewal happens only when an application request needs a new access token.
A local credential refusal occurs before HTTP submission and records a refused operation, not an unknown server outcome.
Failed renewal or login expiry requires explicit login and submission. Quitting clears local credentials and requests server logout.

### Control sessions

Every org in `registry.orgs` has the control audience `urn:wamn:control:<org>`, and nothing configures it.
Discovery offers it to a user who holds `org-admin` in that org or `project-admin` in a project of that org, also when the request names a project, so an application shell offers it beside the environments.
A control session comes from a password login only, because the PAT exchange refuses a control audience.
It carries no roles.
For each request, the control host reads the password login of the session, the active principal and its `org-admin` and `project-admin` roles in the org.
A revoked role therefore refuses the next request and the next renewal with 401.

### Consistent session access

Sessions and PATs use the same operation permission check: the caller holds the entry.
Every user session receives the same current authority checks without operation-specific password prompts.
Renewal stays automatic during active use and never extends the absolute login deadline.

The identity reader needs SELECT on `identity.password_logins` in addition to its existing identity tables.
Session-only hosts also require their scoped `WAMN_SYSTEM_URL` identity reader.

## Password enrollment foundation

The [identity library](../../crates/identity/platform/src/password.rs) supports password storage, invitation enrollment, and password authentication.
The identity service exposes password HTTP endpoints. Receiving supports hidden terminal enrollment and password login.

The [login storage library](../../crates/identity/platform/src/password_login.rs) supplies database primitives for renewal and revocation.
The HTTP service and terminal use these primitives for renewal and server-side logout.
Each login binds one user, issuer, and exact environment audience.
Its authentication time and eight-hour absolute deadline remain fixed.
Successful renewal extends the inactivity deadline by 30 minutes, capped at the absolute deadline.

The database stores only hashes of random, single-use renewal credentials.
Reusing a consumed credential revokes its login family, the sequence of replacement credentials for one login.
The caller commits that refusal so revocation persists.
Every login and renewal repeats the current membership, environment instance, tenant-user status, and assigned-role checks.
Credential rotation and access-token signing share one transaction after those checks.
The service delivers credentials only after that transaction commits.
Access-token expiry cannot exceed the login's absolute deadline.
The caller locks the principal before password verification and keeps that lock through login creation.
Renewal, logout, and password reset use the same principal lock.
Principal disablement revokes its families through a database trigger, so reactivation cannot restore them.

The issuer cannot change a login's principal, audience, authentication time, or absolute deadline.
All writes require an actor, and credential tables have no row-image history.
Consumed credentials remain until absolute expiry.
Global request admission removes at most 100 expired families per call, including their credentials.

Enrollment accepts an active, unenrolled user principal and a matching, unexpired invitation.
The library locks that principal, creates its password, and consumes every outstanding invitation in one database transaction.
Issuance records the authorized operator. Successful enrollment records the invited user.
The existing principal, memberships, and PATs remain unchanged.
The identity issuer has narrow grants for enrollment and authentication.
A restricted database function locks a principal without granting permission to change that principal.

Invitation secrets contain 32 random bytes and expire after 24 hours.
Reset secrets use the same implementation with a separate prefix, reset purpose, and 15-minute expiry.
The database stores their SHA-256 digests and explicit purpose, never the bearer secret.
Password hashes use Argon2id version 19 with 19 MiB, two iterations, one lane, and independent 16-byte salts.
These parameters meet the [OWASP minimum profile](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html#argon2id).
One shared worker budget permits two jobs, with no waiting queue, for 38 MiB of Argon2 working memory.
Cancellation retains the permit until the blocking worker ends.
Stored hashes with another cost profile refuse before expensive work starts.

Enrollment requires 15 Unicode characters and accepts at most 1,024 UTF-8 bytes without trimming or normalization.
The service limits password requests to eight per replica and HTTPS connections to 128 per replica.
Database counters enforce 60-second windows across replicas: 120 requests globally, 20 per source address, and five per account.
Enrollment uses the principal ID for its account counter. Login uses a digest of the normalized email.
The source is the TLS peer address, never a forwarded header.
Refused attempts do not extend the window. Counters older than two minutes are removed during admission.
Password requests expire after ten seconds and accept at most 8,192 body bytes.
Operators behind a shared proxy also share its source limit.

`POST /invitations` requires the existing verified operator certificate and a `principal_id`.
The service sends the one-time secret through Resend to that principal's stored email.
A successful provider response means accepted for delivery, not confirmed inbox delivery.
The service makes no automatic send retry and reports failed or uncertain sends as unavailable.

`POST /users` requires the operator certificate and accepts `email` and `display_name`.
It creates the user of the email as `wamn:provisioning`, or reuses the user that has it, and answers 200 with `principal_id` and `enrolled`.
The email folds to lower case and is also the subject. `enrolled` is true when the user has a password.
An email that the identity rules refuse answers 400 with the reason, and a disabled user answers 409.

`POST /password/enroll` accepts `principal_id`, `invitation`, and `password`.
`POST /password/session` accepts `email`, `password`, and `aud`.
It reuses the PAT exchange's membership, current environment, active tenant user, role, and signing checks.
Its response contains `access_token`, `token_type`, `expires_at`, `renewal_token`, and `login_expires_at`.
The two expiry fields contain Unix seconds. `token_type` is `Bearer`.

`POST /password/renew` accepts `renewal_token` and `aud` and returns the same response fields.
Each successful request replaces the renewal credential. A lost response requires another password login.
There is no retry grace period or automatic application request replay.

`POST /password/logout` accepts `renewal_token` and `aud` and revokes that login family.
It returns HTTP 204, including when the credential is already unusable.
`POST /password/logout-all` accepts the same fields and requires a currently usable renewal credential.
It revokes all password login families for that user and returns HTTP 204.
Neither operation requires continued application access. Neither revokes PATs.
Previously issued password tokens fail new admission after logout commits.

`/password/session`, `/password/renew`, and `/password/logout` accept an optional `carrier`, `"bearer"` by default or `"cookie"`.
With `"cookie"`, the reply body holds only `expires_at` and `login_expires_at`, and the tokens travel in three cookies.
`__Host-wamn-session` holds the session token. It is HttpOnly, with `Path=/`.
`__Host-wamn-csrf` holds a new random CSRF token. The page reads it, so it is not HttpOnly.
`__Secure-wamn-renewal` holds the renewal token. It is HttpOnly, with `Path=/password`.
All three are `Secure` and `SameSite=Strict` and carry no `Domain`.
The session and CSRF cookies live as long as the token. The renewal cookie lives as long as the login.
Renew and logout read the renewal token from its cookie and refuse a `renewal_token` in the body.
Logout clears all three cookies with `Max-Age=0`. `/password/logout-all` accepts only the bearer carrier.

`POST /password/recover` accepts `email` and shares the global, source, and account limits.
The service sends a reset secret only for an active user with an established password.
After admission, the account-dependent path has a six-second deadline and a fixed response delay.
Known and unknown accounts receive HTTP 202 with `{"status":"if_eligible_email_will_arrive"}`.
That response does not promise delivery. Provider failure does not expose account existence.
The service makes no automatic mail retry.

`POST /password/reset` accepts `email`, `secret`, and `password`.
It requires a matching, unexpired reset credential and the existing password policy.
One transaction replaces the password, consumes all outstanding invitation/reset secrets, and revokes all renewal families for that user.
The service sends a password-change notification after commit and creates no session.
HTTP 200 reports `{"status":"password_reset","notification":"accepted_for_delivery"}` when the provider accepts the notification.
If notification fails, the response reports `"notification":"unavailable"`. The password change remains committed.
Normal login uses the replacement password. Existing PATs retain their separate revocation path.
The terminal supports recovery through the same endpoints and requires normal login after reset.
The web shell does the same at `/recover` and `/reset#<secret>`, and then opens sign in.
For mailbox loss, an authorized administrator updates the existing principal's email through the [operator procedure](../operations/deployment.md#mailbox-loss-recovery).
That transaction consumes outstanding email secrets and revokes renewal families. The user then uses normal recovery at the replacement address.

`POST /password/environments` accepts `email`, `password`, optional `org` and `project` filters, and optional `limit` and `cursor` members.
It uses the control lists' paging rules above: limits 1 through 100, default 100, and whole results when both paging members are absent.
After password authentication, it returns authorized control audiences, then authorized application audiences, each ordered by audience.
Its response retains `environments` and adds nullable `next_cursor` and `total` members with the same meaning as the control lists.
A control entry carries only `aud` and `org`. The project filter does not exclude control entries.
The cursor key contains the kind (control 0, application 1) and audience.
Its scope binds the authenticated principal and the exact optional org and project filters.
Invalid limits or cursors return HTTP 400 with `{"error":"password request refused"}`.

Discovery stops after enough authorized candidates establish the page and continuation.
Authorization can reject candidates, so a page can examine more candidates than its limit.
Every page authenticates again and uses the existing five-attempt login bucket. HTTP 429 retains `Retry-After`.

Discovery returns no session, PAT, application address, or database credential.
It shares the login throttle and request deadline with session issuance.
An unavailable authority read fails the whole request instead of returning an incomplete list.
Session issuance repeats the access checks after selection. A discovery result grants no access.

The issuer never supplies application addresses.

The development environment owns a separate identity process from startup until explicit teardown.
Application builds preserve that process, its identity database, and its signing keys.
This process ownership adds no external identity provider.

Unknown accounts and incorrect passwords receive the same unauthorized response.
These routes are enabled only when the service has a Resend key and sender.
The terminal serializes renewal and preserves the original absolute deadline with both wall-clock and monotonic expiry checks.
It clears credentials before renewal I/O, so failure or cancellation requires another login without retrying a consumed credential.
Logout clears local credentials even when server revocation cannot be confirmed.
Unknown and unenrolled accounts perform the same bounded hashing profile as existing accounts.
Password buffers erase their owned bytes on drop. Diagnostics redact passwords and invitation secrets.

## Session keys

The host uses an explicit HTTPS issuer endpoint and nonempty trusted CA configuration.
It does not discover endpoints, follow redirects, use ambient proxies, or add ambient trust roots.
A fetched key set replaces the preceding complete set.
Unknown keys cannot obtain authority from another issuer.

Key freshness is bounded to 300 seconds from request start, including the response's HTTP `Age`.
The cache permits one fetch per issuer at a time, with starts at least one second apart.
A fetch permits five seconds and 65,536 response bytes.
A stale set refuses authentication even for a previously known key.
Final admission cannot extend freshness after slow permission reads.

The identity service publishes a public key before activating it for signing.
A transaction lock establishes the stop-signing boundary before retirement.
Public keys remain available for 930 seconds after that boundary.
An emergency key removal does not activate a replacement.
Previously cached keys can still authenticate for their remaining 300-second freshness window.

Private signing keys remain in the system database and identity service.
The public endpoint exposes only public key material.
The [key cache](../../crates/platform/runtime/src/session_keys.rs) and [key owner](../../crates/identity/platform/src/session_keys.rs) define these bounds.

## Operator clients

Generated screens use the [client contract](../../crates/schema/generator/src/client_ir.rs), derived from admitted declarations.
Operation kinds come from the manifest.
Record links come from declared relations, keys, and compatible reads.
The generator never guesses a record from a field name ending in `_id`.

The [screen plan](../../crates/schema/generator/src/client_plan.rs) holds the screen rules that every UI target applies.
It gives each callable operation one role: table, detail, form, delete, or none with a reason.
It also states the columns, the operator inputs, the row source, the paging, and the screens that one row opens.
It carries interaction meaning only, so no layout, styling, or framework concept enters it.

A package can also generate [TypeScript bindings](../../crates/schema/generator/src/client_ts.rs) for a browser.
It opts in with a `client_package` name in its manifest, and a package that declares none generates no TypeScript. Nothing is published to a registry, and the name is what a workspace imports.
The bindings carry a request type, a result type, the published route and one function for each public operation.
A private event handler is absent, and an operation the release does not publish carries its types without a function.
The application supplies a transport that owns the URL and the credential.
That transport also classifies one response into a completion, a partial completion, a refusal or an uncertainty.
The contract stays snake_case on the wire, and the TypeScript members are camelCase.
The generator decides every member name and emits a field map beside each operation, so the browser holds no name rule.
A key that no map declares keeps its spelling, so the inside of a `json` value is never renamed.
A bounded list returns its rows under `rows`, and a page returns them under `item` beside a `nextCursor`.
An `int64` and a `numeric` are strings in TypeScript, because that is what the wire carries and a number loses precision above 2^53.
A field that declares a closed value domain types as a union of its literals.

The [web runtime](../../web/runtime/) is the hand-written package that the bindings import.
It is TypeScript source, it builds nothing, and a generated package declares it as its one dependency.
It implements the transport: the URL, the credential the application supplies, the request envelope, and the classification of one reply.
The runtime tests read the cases in `web/runtime/test/classification-cases.json`.
The browser transport also sends one write that carries many items, each with its own request identity, and reads one outcome for each item.
A 200 reply with one outcome for each item classifies each item by the rule of one item, so a refusal marks only its own item.
Any other reply gives every item the same outcome. The table's `many_cases` state this rule.
The browser trusts the platform for the values inside a reply, so it holds no schema validator and no copy of the wire spelling rules.
The runtime also holds what a generated component calls: the state of one read, the members of a draft, and the display text of a declared value.

Each transport holds a [read store](../../web/runtime/src/readCache.ts), keyed by the operation and the canonical GET target.
Equal reads in flight share one request, and a list stays fresh for its `max-age`.
A stale entry with an ETag sends `If-None-Match`, and a 304 reuses the stored reply.
The store keeps only a 200 reply that classifies as completed or refused.
A read that sends the store's tag calls fetch with `cache: "no-store"`, so its 304 reaches the store.
A read that the store does not hold, as after a reload, calls fetch with `cache: "no-cache"`. The browser revalidates its own copy, and a 304 saves the body.
Every operation contract lists its `relations`, and the generated `ResponseContract` carries them as `reads` and `writes`, each a list of `schema.table`.
A read names every relation that it selects from, and a history table counts as its model table.
A write names each relation that it inserts into, updates or deletes from. A relation that it only locks is not written.
A write marks stale only the stored reads that share a relation with it, by the one rule `writeTouches` in the runtime.
A write whose contract names no relation, or a read whose contract names none, counts as touching, so that pair behaves as before.
A materializer runs after the write's response and can write other tables, which the browser cannot see. A stored read of such a table stays fresh until its `max-age` ends, and its ETag then reads the change.
A new CSRF cookie empties the store, so one session's reads never answer the next session.
The transport also calls each `onWrite` listener with the write's relations after the write settles.
A generated detail, a table, an edit cell, the record text in a table's reference column and a form's selector list listen through `afterWrites` with the relations of their own read.
Each reads again only after a write that touches them, so an unchanged read costs one 304 and an untouched read costs nothing.
A form's own record read does not read again, because the form sends the revision that it read when it opened.

A package that generates TypeScript also generates [wfe descriptions](../../crates/schema/generator/src/client_component.rs) for its supported screen roles.
Descriptions contain presentation data for tables, details, forms, and deletes.
The [Receiving web application](../../apps/wamn_receiving/web/README.md) binds those descriptions to requests, validation, reference selectors, and navigation.
The generated TypeScript bindings continue to use the browser runtime.

A request type declares writable members, because a caller builds a request and a form library writes into it. A result type keeps its own read only.
Descriptions state no route or navigation. The application decides what to open.

The [platform UI](../../apps/platform_ui/README.md) owns the shared Solid 2 shell.
Its [package](../../apps/platform_ui/package.json) uses wfe components, forms, and generator descriptions.
Receiving uses this shell with generated descriptions.

The [shell](../../apps/platform_ui/src/shell.tsx) reuses `web/runtime` for the cookie session and transport.
Each application supplies its route table, grouped into sections. The generator emits no navigation list.
Each screen, route, and action names its operation through a generated route constant.
The shell reads `permission.mine` for an application session and `control.mine` for a control session.
It filters screens and actions by those permissions. An application administrator sees every application operation.
This filtering controls presentation. The server remains the authority for each request.

The [address owner](../../apps/platform_ui/src/address.ts) keeps the audience in the first path segment and the record keys in later segments.
A screen receives the transport, address parameters, query values, permitted actions, `open(path)`, and `close()`.
The shell fills action paths from the address parameters.
Reloading a screen renews its cookie session without browser storage. A screen without a session asks for the password at that address.
The [identity pages](../../apps/platform_ui/src/identity.tsx) offer reachable audiences after sign-in and open a single audience automatically.
An account with no audience sees "No access has been granted." and receives no session.

The [page components](../../apps/platform_ui/src/page.tsx) wrap dashboards in wfe `Theme` and `DashboardShell` at viewport height.
Sign-in, invitation, recovery, reset, and no-access pages use standalone `EntryPanel` pages.
Unknown or forbidden addresses and missing records use the same standalone layout, with a heading, explanation, and return link.
These pages have no sidebar or breadcrumbs.
An audience with no available screens keeps its dashboard header and offers a link to audience selection, without a navigation pane.

The [left pane](../../apps/platform_ui/src/control-navigation.tsx) follows the hierarchy organization > project > environment.
Its context switcher searches sibling choices and returns to the parent. Expandable groups expose the projects and environments that the caller administers.
The address controls the context across direct links, reloads, and browser history.
The [application layout](../../apps/platform_ui/src/layout.tsx) adds role context with Permissions and Users links.
The [administration routes](../../apps/platform_ui/src/administration.tsx) expose Roles, Users, and Permissions overview according to held operations.

The dashboard header carries breadcrumbs, the links that show the current location, and the User menu with Sign out.
[Layout](../../apps/platform_ui/src/layout.tsx) renders every breadcrumb as literal ALL CAPS text, including record names.
Identity pages and audience selection omit breadcrumbs.
Links navigate, and buttons act. [RecordLink and NavigationLink](../../apps/platform_ui/src/page.tsx) use native anchors with real addresses and preserve modified clicks.

A table that can grow fills its panel, places its actions in `toolbarActions`, and pages its rows.
The [table screen style](../../apps/platform_ui/src/page.css) fills the available space without outer padding.
[Table routes](../../apps/platform_ui/src/projects.tsx) use `scroll="content"`.
[Records](../../apps/platform_ui/src/records.tsx) supplies wfe `TableView` with toolbar actions and pagination, the controls that select a page of rows.
The project, user, environment, member, control-project, and audience lists request `limit: 25` from their servers.
The application keeps prior cursors for Previous and replaces rows only after a successful request.
A scope change or reload starts a new traversal. Pending requests disable the page controls.
The UI passes `totalRows` only when the server supplies a non-null total. Unknown totals show only the current page number.
Whole-result reference tables retain local paging over their complete arrays.
The shell bootstrap keeps a whole `control.mine` call, and environment lookups by name keep whole calls.
Audience selection opens a single audience automatically only when the first response has no continuation.
[TableView](../../web/wfe/packages/generator/src/TableView.tsx) passes these bindings to the wfe table.

Each column shows one fact. Growing lists appear as counts in parent rows.
[Environment tables](../../apps/platform_ui/src/environment.tsx) expand read-only reference tables through `rowDetails`, without new routes.
Tables with actions retain their own screens.
Data entry uses a separate form screen. [Create project](../../apps/platform_ui/src/projects.tsx) and [Grant membership](../../apps/platform_ui/src/membership.tsx) use `FormView`.
Successful submission and Cancel return to the owning table. A refusal leaves the form open with its error.
Modals serve confirmations only, as in [ConfirmAction](../../apps/platform_ui/src/confirm-action.tsx).

Inline row actions use icon buttons. Delete and Revoke use the wfe `cross` icon.
[ConfirmAction](../../apps/platform_ui/src/confirm-action.tsx) keeps the full action label in `aria-label` and `title`.
The [wfe icon](../../web/wfe/packages/components/src/components/Icon.tsx) is decorative, so the button supplies its accessible name.
State toggles keep text, including Activate and Grant org admin in [Users](../../apps/platform_ui/src/users.tsx).
Toolbar actions and form buttons also keep text.

The platform UI shell runs on `@wfe/shell` at the wfe commit `fdeaacc55e5e6582ee4d117e656c1d29a7be8b84`.
[Layout](../../apps/platform_ui/src/layout.tsx) binds the accepted audience and page through `defineShell` and `ShellView`.
Its header override keeps the literal ALL CAPS breadcrumbs and User menu without another heading or audience selector.
Its navigation override keeps the organization, project, and environment hierarchy and the application role context.
An empty audience sets `navigation={false}` to omit the pane and its toggle.
The application owns addresses, permission filtering, and page actions.

The shell calls the release under `/api` through its [transport](../../apps/platform_ui/src/shell.tsx).
The platform UI calls the control host through `/api/wamn_control` at `control.wamn.dev`.
Its [deployment](../operations/deployment.md#platform-ui-files) owns the edge routing and the org-only scope.

A revision-bearing operation needs a declared compatible record read and revision mapping.
Without that mapping, the screen requires ordinary Rust composition and blocks submission.
A user cannot type a revision or obtain a guessed read route.
Query columns come from that query's result descriptors, not a union of model fields.
Generated screens show the [record history](data-access.md#record-history) stamp columns `created_at`, `created_by`, `updated_at`, and `updated_by`.

Fields separately declare whether a property is required and whether its value can be null.
Editors retain `Absent`, `Null`, and `Value` according to those two facts.
Nested and repeated fields retain their declared bounds.
Unsupported input types block submission.
Unknown output types remain display-only and appear as opaque values.

Record screens show the returned `created_by` and `updated_by` IDs.
A display name comes from the application's own SQL, which joins `platform."user".display_name` where it needs a name.
The platform adds no query after the result.
The history view keeps its existing behavior.

The served route supplies its terminal response contract and whole-operation replay guarantee.
A composed route does not inherit replay safety from an inner command.
Unknown replay information grants no retry.
An operation without an HTTP route remains visible as unexposed.
Event handlers do not become operator actions.

The shared request builder canonicalizes typed UUID, timestamp, and numeric input through `wamn-client`.
The invocation layer canonicalizes bytes and does not reinterpret field types.
The current client submits one outer envelope item, while nested repeated fields remain supported.
Cursors remain opaque and reset when filters or sorting change.

A pending command blocks a second submission.
A confirmed refusal without completion keeps the draft editable.
Success and confirmed partial completion spend the submission.
Unknown errors, malformed responses, and transport failures retain uncertainty about completion.
A `schema-invalid` body whose `data` holds only a string `pointer` is a refusal before delivery, and the client reports that pointer.

Uncertainty belongs to the whole submitted intent.
A later retry refusal does not clear uncertainty from an earlier attempt.
Safe retry uses the captured body, key, time, and expected revision byte-for-byte.
Authorization headers are derived again.
Only the served write-log replay contract permits this retry.
A state command or effectful composition requires a refresh before a new intent.
Abandoning local intent cancels no server work.

The client binds to the served URL, host, and target instance.
A replacement target resets records, revisions, cursors, drafts, and pending state.
Submissions remain blocked until the matching replacement succeeds.
A rebuild that preserves the earlier activation leaves that session usable.
An invalidated session cannot replay old-target mutations against a replacement.
Process restart prevents old responses from reaching the new session.

The generated client contains no endpoint, host, or token.
Development supplies them from the served target.

## Process lifecycle

Host readiness requires a real native command-loop beat.
Queue liveness follows durable polls and successful lease renewals in each host replica.
No timer creates a synthetic beat.
An absent first beat fails through the normal silence budget.

The host drains readiness, stops queue claims, and starts native HTTP shutdown together.
The same admission signal refuses further HTTP requests on existing connections.
One shutdown signal stops HTTP admission and queue polling, bounds active work in both paths, and then performs shared cleanup.
Ingress failure, probe failure, or command-task failure follows the failure cleanup path.
The process bounds plugin coordination, auxiliary task cleanup, telemetry flush, and final runtime shutdown.
Cleanup timeout remains an error and can leave plugin cleanup incomplete.
The HTTP-stop allowance bounds coordination and does not guarantee native completion.

Queue shutdown bounds its current turn and auxiliary cleanup.
A turn beyond the budget retains the existing durable lease for recovery.
SIGTERM and SIGINT share the host cleanup budget. The budget retains the native drain and plugin shutdown allowances.
An aborted call loses its invocation authority, and native teardown releases its store.
After the lease expires, recovery takes a new lease generation. The old generation cannot complete the run.
The [runtime tests](../operations/running-tests.md#host-and-package-runtime) cover the retained host and package paths.

OpenTelemetry records invocation and effect spans, request timing, and delivery outcomes.
A head sampler decides whether to export a trace at its start.
The existing head sampler decides at the request's entry span, the upstream `handle_http_request`.
The `wamn-host` subscriber (`services/host/src/observability.rs`) then creates no other span in an unsampled trace.
It wraps the layers that wash-runtime installs. The wrapper refuses a span while the active OpenTelemetry span is a local, unsampled one.
A refused span reaches no layer, and the tracing registry stores nothing for it. Span call sites, WAMN and upstream, do not check sampling.
An unsampled request therefore creates only its entry span.
A caller's remote trace context does not stop the entry span, so the sampler still makes the decision.
Sampled requests retain their span names, fields, levels, and parent links. Log events remain unchanged, but console lines in an unsampled trace name no span below the entry.
Without a valid context, startup and local tracing keep their existing behavior.
Native dispatch and spawned request tasks carry the entered entry span, so they read the same decision.
Operational logs use INFO. Hosts send request traces to the platform OpenTelemetry collector.

The [deployment guide](../operations/deployment.md) owns probe ports, grace periods, and rollout commands.
The [native alignment page](native-alignment.md#current-limits) retains the unresolved operator retry and liveness limits.

## Change feed

A generated `query` of a model with a history table and a revision column has a change feed ([wamn offline and sync](../plan/wamn-sync.md)).
The feed is the `query` operation with its feed part, `feed: { cursor, limit }`, on the same GET route, so it has the list's grant and no permission row of its own.
Its value is `{ rows, next_cursor, complete }`. Each row carries `id`, `row_version` and `deleted`, and a row that is not deleted carries `data`.
The page of such a `query` also carries `cursor`, which starts the feed at the page's snapshot.
The cursor is opaque: the URL-safe base64 of the model name and a snapshot xmin, which [`feed.rs`](../../apps/platform/data/access/src/feed.rs) encodes.
A cursor of another model, or one that does not decode, refuses with `invalid_input` on `cursor`. A cursor older than the oldest entry a `P<n>D` retention keeps refuses with `cursor_expired`.
