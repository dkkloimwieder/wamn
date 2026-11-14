# Request execution

A request enters through a declared route and keeps its caller identity throughout execution.
The host binds the route to one component operation or to an admitted wiring in the supplied release.
A route target calls its operation once, with no graph walk and no retry.
For a wiring, the router decides which operation runs next. Native dispatch executes each operation.
A hot HTTP request creates no run or queue row.
The warm pool holds reusable instances. Overflow calls use fresh stores.

## Routing and availability

The HTTP ingress path authenticates the caller before invoking application code.
The host compares the route, operation, permissions, wiring, and release facts before dispatch.
The engine owns HTTP authentication and the generated `wamn:operation` binding.
The separate HTTP component reads released route data through `wasi:config` and calls typed application exports through that binding.
The cloud implements the operation boundary over PostgreSQL. The edge implements it over its own SQLx SQLite store.
`RouteAuthenticator` reads the credential of a protected route.
The cloud uses [`PlatformRouteAuthenticator`](../../crates/platform/authentication/src/route_authentication.rs), and the edge uses its local session verifier.
Publish decides authorization, and the runtime trusts the loaded release.
An application's components compose at build into one component, so a call inside an application reaches no host.
The host begins a transaction for an operation whose admitted operation declares one, whatever SQL it holds. The required permission is the entry's own operation, and nothing derives into it.
The host makes sure that the original caller holds the entry, once, before the entry runs. A caller holds exactly the permission rows of its roles.
Component membership alone grants no permission to call an operation.

An application host makes sure of the status of its environment for each request, before the auth policy of the route.
It reads its row of `platform.environment` through the compiled SQL login of its project. That login inherits `wamn_app`. No row means active.
The host keeps the answer in its [auth memory](#auth-memory).
While the status is `inactive`, every route answers 503 with the code `environment-inactive`, and an anonymous route does too.
A failed read answers 503 `authentication-unavailable`.
The control host and an edge box read no status.
The [status routes](#host-run-routes) of the control host write the row.

An attachment with auth policy `none` or `signature` has no principal, so it cannot write.
If the reachable wiring of an anonymous attachment holds a registered operation or a transactional statement, [release publish](../../crates/control/publish/src/publish_release/components.rs) refuses it.

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

The method of a route follows from the type of its operation, and no author writes it.
Publish writes GET for a route to a `get`, `query` or `projection` operation, and POST for every other route and every wiring.
A GET carries exactly one request item in its query string.
The router reads its bounded body for authentication, but takes the operation input from the query string.
Each top-level member is one parameter, and its value is the compact JSON text of the member.
The client encoder sorts parameter names and uses one escaping format so its requests share cache entries.
The parser accepts any parameter order and valid escaping, and parses each value as JSON.
The router refuses malformed JSON and targets longer than 8 KiB with HTTP 400 and `invalid-target`.
The [CLI encoder](../../services/ctl/src/read_query.rs) writes the query. The [HTTP parser](../../apps/platform/ingress/http-route/src/read_query.rs) reads it.

The host derives the `Cache-Control` value of each read route from its operation type and auth policy when it serves the route, and the router only writes it.
A `get` sends `no-cache`, and a `query` or `projection` sends `max-age=10, stale-while-revalidate=60`.
A `query` with its feed part (the [change feed](#change-feed)) sends `no-cache`.
Each is `private`, unless the auth policy of the route is `none`, and then it is `public`.
A successful read also sends `Vary: Authorization, Cookie`.
Every other response sends `Cache-Control: no-store`: a write, any status other than 200, and any request that carries `x-wamn-csrf`.
[`read_cache_control`](../../crates/platform/engine/src/flow_http_routing.rs) owns the values.

## Signed file results

A package marks a custom operation's UUID result field with `result.signed_reads`.
For example, `"signed_reads": {"document": {"lifetime_seconds": 60}}` marks the declared result field `document`.
Omitting `lifetime_seconds` selects 900 seconds. The package condition refuses zero, longer lifetimes, unknown fields, and fields with another type.
Only an operation with a permission can declare a signed read.

Admission carries the marking in the main output schema under `x-wamn-signed-reads`.
The release pins that schema in the component descriptor.
After the operation returns, the host replaces each marked file UUID with a signed URL.
The host signs under the operation's grant and the file's application and connection generation.
A signing failure refuses the whole result.
A direct file GET still refuses private files, including requests from administrators.

The host signs marked application results after commit or replay, using the retained invocation authority.
These responses use `no-store` and no ETag because their URLs expire.
The write log retains UUIDs. Each replay signs fresh URLs after the stored result returns.
Typed native calls use the same signing step before the host returns their result.
The guest interface stays unchanged.

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
Native ingress serves the control routes directly. Control loads no HTTP guest.
Control roots have no release record.
Publish refuses an authored attachment under `/wamn_control/`.

[`HostRouteDelivery`](../../crates/execution/host/src/host_route.rs) serves the host routes of the loaded release.
It refuses an attachment outside the native platform endpoints.
The caller must match the attachment, and the route makes sure of its operation grant as any route does.
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
| `wamn-control:role/list@0.9.0` | `roles`: every role of the project, `admin` included. |
| `wamn-control:role/create@0.9.0` | Takes `role` and creates the empty authored role. Answers `created`. |
| `wamn-control:role/delete@0.9.0` | Takes `role` and deletes the authored role with its assignments and permissions. Answers `deleted`. |
| `wamn-control:permission/list@0.9.0` | Takes `role`. Answers `operations`: each operation the release serves and each stored row of the role, with `served`, `grantable`, `admin_only` and `selected`. |
| `wamn-control:permission/grant@0.9.0` | Takes `role` and `operation`, and writes one permission row. Answers `rows_added`. |
| `wamn-control:permission/revoke@0.9.0` | Takes `role` and `operation`, and removes that permission row. Answers an empty object. |
| `wamn-control:user-role/grant@0.9.0` | Takes `user_id` of an application user and `role`, and gives the role. Answers `granted`. |
| `wamn-control:user-role/revoke@0.9.0` | Takes `user_id` and `role`, and takes the role. Direct `admin` grants and revokes refuse. Administration comes from the system project or org role. Answers `revoked`. |
| `wamn-control:number-counter/set@0.9.0` | Authority `Granted`. Takes `application`, `sequence`, `counter`, `key` and the `int64` `value`, and sets that counter. Answers `next_number`. Refusals: `invalid_input`, `sequence_not_found`, `counter_not_found`, `counter_assigned_per_key`, `number_live` and `idempotency_conflict`. |
| `wamn-control:workflow/start@0.9.0` | Authority `Granted`. Takes `workflow_id`, `input_json` and `idempotency_key`, and admits one run of the enabled version of that [environment workflow](#environment-workflows). Answers `run_id`. Refusals: `invalid_input`, `active_role_required`, `workflow_not_enabled`, `workflow_not_served` and `idempotency_conflict`. |
| `wamn-control:connection/authorize@0.9.0` | ProjectAdmin authority. Takes `project`, `env`, and `name`. Returns `authorization_url` for an authorization code connection. |
| `wamn-control:control/mine@0.9.0` | `org_admin`, and projects reached by the current org, environment or project grants. `project_admin` marks authority over the whole project. |
| `wamn-control:user/list@0.9.0` | `users`: each member of the org with `principal_id`, `email`, `display_name`, the org-local `status` and `org_admin`. An `org-admin` reads it, and so does a `project-admin` of any project of the org. |
| `wamn-control:user/invite@0.9.0` | Takes `email`, `display_name` and the optional `org_admin`, `project_admins` and `memberships` (`project`, `env`). Answers `principal_id`, `enrolled` and `invited`. |
| `wamn-control:user/activate@0.9.0` | Takes `principal_id` and makes the org membership active. It restores no access. |
| `wamn-control:user/deactivate@0.9.0` | Takes `principal_id`, removes the member's environment memberships, project roles and org roles in the org, then makes the membership inactive. |
| `wamn-control:project/list@0.9.0` | `projects`: projects of the org as `{name, state}`. |
| `wamn-control:org-admin/grant@0.9.0` | Takes `principal_id` of an active member and writes only the explicit `org-admin` grant. Authorization derives access below it. |
| `wamn-control:org-admin/revoke@0.9.0` | Takes `principal_id` and removes only `org-admin`. Other explicit grants stay. |
| `wamn-control:environment/list@0.9.0` | Takes `project`. Answers `environments`: environments of the project as `{name, state}`. |
| `wamn-control:environment/status@0.9.0` | Answers the shared host rollout, workload conditions, resident releases, and unfinished-run pins for one project of an environment. Adds nullable counts `users_with_role`, `roles`, and `memberships`. |
| `wamn-control:connection/test@0.9.0` | Requires project-admin authority. Takes `project`, `env`, integration `name`, candidate `parameters`, optional `credential`, and optional `version`. Answers `{ok: true}` or `{ok: false, error}`. Writes nothing and stops after ten seconds. |
| `wamn-control:project/create@0.9.0` | Creates a project with no environment. |
| `wamn-control:release/profile@0.9.0` | Takes `project`, `env` and the `digest` of a release installed in that environment. Answers `operations`: for each export of the release's components, the members it `reaches`, each with the exports it holds, `imprecise` when it is a base member's whole statement set, and per statement the relations it reads, writes, deletes and locks, derived from the release's SQL over the environment's relation columns, and `roles`: each role of the environment with the `operations` it holds, `admin` every operation of the release. A view read reports the view columns and each base table with the columns that the view depends on through `pg_depend`. Writes nothing. |
| `wamn-control:member/list@0.9.0` | Takes `project` and an optional `environment`. Answers `members`: each user with a membership or `project-admin` in the project, with `principal_id`, `email`, `display_name`, `org_admin`, `project_admin` and `environments`. |
| `wamn-control:member/grant@0.9.0` | Takes `project`, `env` and `principal_id` of an active member, and writes the membership of that environment. |
| `wamn-control:member/revoke@0.9.0` | Takes `project`, `env` and `principal_id`, and removes the membership of that environment. It refuses while org, environment or project administration covers the membership. |
| `wamn-control:project-admin/grant@0.9.0` | Takes `project` and `principal_id` of an active member, and writes only the explicit `project-admin` grant. |
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

The control service reads the shared host Deployment and application WorkloadDeployments from the org namespace.
It reads `/_wamn/residency` on each live host through the Kubernetes pod proxy.
The host reports each project's loaded application releases and unfinished runs, including releases that failed to load.
An unavailable project remains unknown and does not hide another project's report.
The `residency` result lists the application, manifest digest, loaded state, and run IDs.
The host carries no single release digest. Application Helm histories remain independent.

An application route runs under the administration credential, in a host-owned READ COMMITTED transaction that binds the caller and the sealed operation id.
A host-run write stamps that sealed operation id in its history entries.
A role or permission write first takes the project lock that `wamn-ctl` and release reconciliation take.
The writes are the functions of [`wamn_platform_identity::application`](../../crates/identity/platform/src/application.rs), which the role verbs of `wamn-ctl` also call.
`user_role.revoke` reads current inherited administration through the host’s identity reader. It derives the answer from the org, environment and project grants.
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
The transaction binds `app.user_id` to the caller and `app.operation` to the sealed operation id, and it makes sure of the role again.
The writes are the functions of [`wamn_platform_identity::org`](../../crates/identity/platform/src/org.rs), which `wamn-ctl invite` and `provision-org --admin` also call.
A refusal is a declared error of its route, with the input member it names in `field`: `invalid_input`, `project_not_found`, `environment_not_found`, `user_not_active`, `user_not_found`, `admin_covered` or `user_refused`.
A write that changes administrative authority or membership also changes the application rows of each environment it covers, one transaction per environment, with the environment's `wamn_administration` login.
Native control reads that login from the installed public connection projection, one file per `<project>--<env>`, at each write. Its component configuration selects `administration_dir`; passwords stay in the owning role Secrets.
Every authority mutation commits its system rows before application projection.
Projection locks `identity.org_memberships` for the principal and org, then reads current authority.
It commits the application write before releasing that system lock.
If an environment fails, the route answers `application_write_incomplete` with that environment and the environments that completed.
The system mutation remains committed.
The next system mutation or `environment::apply` repairs application rows from current authority.
The four status routes need `org-admin`, and they write `registry.environments.status` through the `control` family, which holds `UPDATE` on that column only.
Each status route also writes the status into the one row of `platform.environment` in each environment, with the same administration login.
An inactivation writes the environments first and commits its system rows last, and an activation commits its system rows first.
Data, membership and role rows do not change with the status, and CDC stays attached.
`user.invite` calls identity `POST /users` and `POST /invitations` with the configured identity operator certificate, through [`wamn-identity-client`](../../crates/identity/client/src/lib.rs).
It writes the membership and the grants between the two calls, and it calls `/invitations` only when the user has no password.
The host refuses an email or a display name that the identity rules refuse before it calls identity, and answers `user_refused` when identity refuses the user.
The control set also serves `application.deploy`, `application.rollback`, `application.stop`, `release.retain`, `schema.diff`, `schema.apply`, `schema.reset`, and `operation.status`.
Each route requires `ProjectAdmin` authority for `project` in the host's organization.
The request names `project` and `env`.
The server builds the command and credential references after it repeats authorization and resolves the target.
Each start route returns a Kubernetes Job name.
`operation.status` compares that Job's target and reads its state and bounded log tail.
The service submits Jobs through the shared runner and keeps no operation history.
The protected `WAMN_DEPLOY_ACTOR` input carries the authenticated principal to the worker.
[Deployment routes](../operations/deployment.md#application-and-schema-operations) defines inputs, credential target, deadlines, and result bounds.

The contracts of the control routes live in [`host_route/control/contracts`](../../crates/catalog/model/src/host_route/control/contracts), and the generator projects them into the TypeScript client `@wamn/control-org-client` in `apps/target/wamn/wamn_control/control/client-ts`.
The control set has its own client because both sets serve `user.list`.

The native control service has no application release or guest connection.
`wamn-control` serves it for one org.
It reads owning role credentials for authenticated administration. An application host never holds the control login.
See [Native control](control.md) for the installer configuration and worker interface.

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
The separate HTTP component owns the tags.

The entry-only `wamn:operation/operation@0.1.0` interface also provides `wal-position()`.
It reads the WAL position on the open operation's connection and refuses `not-begun` outside an operation.
The [operation boundary](data-access.md#the-write-log) owns its transaction lifetime.
The separate route component computes ETags and handles conditional reads for base routes.
The routing provider also projects the ETag inputs from the loaded release.
Those inputs declare the release digest and either the result's revision field or the sorted relations for a list.
A get without a revision field and a list without relations carry no ETag inputs.
The guest receives the type and fields as data, without application-specific names in the route code.

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
The generated typed export validates the remaining input.
The memory of a read is bounded by the statement's declared limit and the platform ceiling.
The test `a_read_of_100000_rows_refuses_at_the_ceiling` in [`route_interface_live`](../../crates/execution/host/tests/route_interface_live.rs) shows the refusal.
The route component invokes the application through the host converter.
The typed query returns its page. The host retains the read's WAL position for a list ETag.
Platform endpoints use native handlers. Workflows and edge calls use the shared operation entry.

The shared entry calls one typed export under one invocation authority.
The operation owner claims each write request before execution and returns stored outcomes on replay.
SQL claims share the transaction of their work. Operations with external effects commit their claims before those effects.
In the cloud the store is the [write log](data-access.md#the-write-log).
The edge owns one SQLx SQLite file with one writer.
Its claims are unique by partition, stable operation reference and caller key. The partition holds the edge organization ID.
The device and HTTP paths use the same admission, claim, dispatch and completion functions.
Device completion stores its sample and outcome in one transaction.
A changed request under the same key is a conflict. A finished key returns its stored outcome.
An unfinished claim answers `in-progress` for 30 seconds, then `outcome-unknown`.
A late completion records its result, and the next retry returns it.
Unfinished claims expire after 30 days, far above the maximum call duration.
The client checks the external system before sending a new key.
Sample refusals have their own local operator decision. The generic intent resolution commands are removed.
A retry that the write log answers commits nothing, so change capture publishes no event for it and no post-commit work runs a second time.
Publish writes the key field (`idempotency`) into the serving manifest route.
The server hashes parsed JSON with sorted object keys. It leaves out `request_id` and the key field.
It preserves scalar spelling and array order. A retry with changed input is an idempotency conflict.

A host or wiring route input that fails its schema returns HTTP 400 with the code `schema-invalid`.
Base routes keep this response for declared page and band limits.
Their generated typed exports refuse other invalid input through the operation contract.
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
One host serves current selections and releases pinned by unfinished runs together.
The host reads current selections from the Helm release projections.
After restart, it also reads unfinished runs and loads their original catalog manifest bytes.
It verifies each digest before preparing that release.
A failed preparation refuses that release and reports its pinned runs. Other release queues continue.
Release preparation does not control process readiness.
Each project deployment, application, and digest has one execution owner.
Applications can select identical release bytes. They share verified bytes and compiled code, while each keeps its own execution authority.
The host prepares releases independently and starts each ready queue immediately.
Each resident release runs file orphan collection. Unloading stops its collector, and host shutdown cancels and joins every collector.
A release stays resident while selected, while an unfinished run pins it, or while a workload or invocation retains it.
Native dispatch reuses compiled component bytes. Untrusted components receive a fresh store, which holds guest memory and resources, for each call.
The deployment owner can select reviewed component digests for native warm-instance reuse.
[Deployment configuration](../operations/deployment.md#trusted-component-reuse) owns this selection and pool sizing.
Application manifests cannot grant that trust.

An application is one composed component, and it loads as its own native workload.
Publication records the separate route component in the release's entry record.
The host starts that pinned component and loads application components separately.
The route component receives release route data through `wasi:config`.
Each request calls the host's operation capability, which invokes typed `run` in the application's store.
The release chart uses a fresh store for each route request.
The host serves platform endpoints natively under the same ingress host.
Local publication records an absolute `file:` reference to a digest-named artifact. The existing local file loader makes sure of its bytes before binding.

No two applications share a store, and wash-runtime links no application to another.
Applications in one release can use different lifetimes.
A call across applications is a workflow call through the host.

Each call receives a new host-owned invocation scope.
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
Admitted runs never substitute a newer connection generation for their pin.
Connection effects refuse a changed generation, a disabled connection, or an unavailable pinned credential.
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
Pools belong to one immutable release or candidate and never cross project or environment boundaries.
A replacement release or deployment trust configuration uses new pools.
Credential tests retain the exact pinned generation rules above.

Pool capacity does not limit request admission or total memory.
A chart host derives its guest memory budget from its cgroup: three quarters of the 768Mi limit, which is 576 MiB.
A process memory guard at 90 percent of the limit refuses guest growth and new workloads in every mode.
The guard samples resident memory once per second, so it is not a hard allocation cap.
The host enforces the guest memory budget and raw-socket egress policy ([native alignment](native-alignment.md#retained-wamn-implementations)).
The pinned wash runtime refuses growth beyond the shared guest budget.
The host flags `--guest-memory-mode` and `--socket-egress` each accept count or enforce.
Both default to enforce. Each linear memory has a 256 MiB ceiling.
The shared guest memory budget remains derived from the container limit.
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
Admission tests the context, node errors, and every nested value.
It refuses store-owned resources at these boundaries.
A composed call inside an application uses the typed entrypoint.
The host converts HTTP and dynamic JSON using the called export's WIT type.
A JSON column or field uses `wamn:node/types.json-value`, a record with one `encoded-json` string.
The converter recognizes this record shape and carries any JSON value through it.
Text fields keep the WIT `string` type.
The converter derives deletion markers from absent change data. It omits that data from a deleted row.
A refusal without details returns an empty object. Absent optional refusal details stay omitted.
Private modules in `wamn-workflow` own deterministic graph decisions.
Before execution, the host admits one invocation record with caller, SQL, identity, causation, effect authority and trace context.
PostgreSQL, HTTP and storage read that record from one registry.
Guest logs use wash-runtime's `TracingLogger` and the host tracing exporter.
Guest context stays a plain attribute.
The host's native logging callback enters the invocation span stored in the active scope before it emits each record.
The record retains signing authority through commit and replay.
Cancellation or shutdown revokes the record. Normal completion releases it after result signing.
The [queue](../../crates/execution/workflow/src/queue.rs) in `wamn-workflow` drives delivery.
`wamn-run-state` owns admission, leases, settlement and pruning as functions over an authorized connection.
`WamnPostgres` authorizes the connection and commits or rolls back each transaction.
The queue owner binds these transactions to `wamn_run`, independently of the application data schema and database default search path.
HTTP admission and queue delivery use separate concurrency bounds, so one workload cannot consume the other's capacity.
Each host replica polls the durable queue and leases work through database leases. Replicas need no wake service or process-local handoff.
Leases and exhausted-run reaping ignore effect-attempt records.
Workflow runs have no event ordering guarantee.

The host opens each identity transaction with `BEGIN ISOLATION LEVEL READ COMMITTED`, so a session default cannot change the level that the [write log](data-access.md#the-write-log) retry depends on.
The guest transaction of a read (get, query or projection) opens with `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY`, so its statements read one snapshot.
For a new logged update, the host runs `BEGIN`, local identity, and the write-log claim in that order before it runs the guest.
A duplicate reads the stored claim and never runs the guest.
The guest waits for its SQL result before it returns the value that the write log stores.
The host then runs the write-log finish, the transactional causation message when present, and `COMMIT` in that order.
A missing finish row causes a SQL error, and the host rolls back.
SQLx runs each statement on the same connection and waits for its result before it sends the next statement.
A cold connection also prepares its statements.
No counter trigger, changed-relation read, or counter reset runs on the write path.

The host binds the executing principal as `app.user_id` in each identity transaction that it opens.
[Record history](data-access.md#record-history) stamps that principal on every write.

- An authenticated caller of a route binds its principal id.
- Native event admission runs as the workflow user.
- An automation delivery binds its admitted service principal.
- A schedule delivery binds the environment's `workflow` user.
- A workflow run binds its execution principal.
- A legacy queue delivery or management candidate case binds `wamn:executor`.

The same transaction binds the executing operation as `app.operation`.
A node binds the operation token of the entry that it runs. A composed callee runs under that token.
An event workflow binds the token of its operation.
A host queue lease, reap, renew, or complete transaction binds the `wamn:executor` credential-class identity.
An absent operation binds an empty value, so a pooled connection keeps no earlier operation.
The [record history log](data-access.md#history-tables-and-the-log-trigger) records the operation in each entry.

The same transaction binds the operation's application name from the `environment.k` map as `app.application`.
A base operation uses the map application that holds its declaring package.
A component outside the composed entry uses the same map.
The `number_counter.set` route binds the requested application after it finds the name in that map.
Other host routes, host queue transactions, and platform reads bind no application.

An absent application binds an empty value, so a pooled connection keeps no earlier application.
The identity is for logs only. [Numbering](data-access.md#numbering) takes the application from host state, not from this identity.

A platform identity transaction that only reads binds no principal and no operation.
A bound principal alone does not move a read out of the autocommit path.

For that path, the host applies the schema and runner before it sends the read.
An absent schema restores the server default, and an absent runner clears the prior runner.
This reuse stores no principal, role, operation, or application identity. Those identity fields stay local to each transaction.

The [session identity owner](../../crates/platform/postgres-capability/src/session_identity.rs) refuses a transactional statement with no executing principal before it reaches PostgreSQL.
That refusal carries SQLSTATE `55000` and the message `actor-required`, as the stamp trigger does.

## Workflows

A workflow is an authored graph that runs off the request path.
The host admits new runs against the current application selection. Existing runs retain their admission digest. Run-state functions own their persistence.
The release loader resolves admitted component facts and verifies their stored effect projections once.
Workflow resolution and host execution share those immutable facts.

- `start` admits one enabled workflow under its execution principal. It writes the run and queue row together. A repeated key returns the first run.
- `park` moves the `available_at` of a queued run to `infinity`, so no lease takes it. The run keeps the status `dispatched`, and a running or finished run refuses.
- `release` returns a parked run to the queue.
- `list` returns the runs of one environment, newest first, with their status and their queued and parked flags.

The `wamn workflow park`, `release` and `runs` commands call authenticated control routes ([queued automation](../operations/queued-automation.md)).
Control binds the authorized connection. Run-state functions perform the reads and writes.

An event workflow is an enabled row of `catalog.workflows` in an environment application.
Its trigger names the source package, entity, operations, and an optional JMESPath condition over the old and new images.
Env apply creates one durable consumer per application, source package and captured entity.
Workflow reads trigger rows and admits all matching workflows for one message through one host transaction.
Admission resolves each graph against that application's current release and freezes its node digests. Each consumer admits only its own application.
The native broker transport acknowledges the message after that transaction commits.

The workflow identity and source event identity form the run key.
CDC identity contains the Postgres system identifier, the timeline containing the commit LSN,
the commit LSN, and the row ordinal within that transaction.
The reader assigns zero-based ordinals before filtering relations, including excluded rows.
The broker message id uses that same coordinate. Derived events retain their logical identity.
A replay retains the original run and release while its run row exists. Pruning ends this protection.
The stream sequence only decides whether an event follows the trigger's `enable_sequence`.

The reader loads publication and slot names from one JSON configuration file.
Provisioning owns the publication, slot and stream declarations.
The reader holds its replication login and broker certificate. It reads no control registration.
Reset stops the reader before discarding its backlog and broker events. Publication takes no system lock.

Each event run executes as the workflow user with its execution role, or `admin` for a callerless invoker.
The run records the delivery id in `invoked_by`.
Admission reads the changed record's history inside the same transaction and records its actor in `invoked_by_user_id`.
It matches the transaction's low 32 bits, the typed primary key, and the operation, then takes the highest history position.
An absent history row or actor leaves the invoking user null.
A former event handler runs as a one-node workflow under a run row.
`list`, `park`, and `release` operate on these event runs.

A package declares no schedule (docs/plan/workflows.md R9).
A schedule workflow is a row of `catalog.workflows` with trigger `schedule` and a five-field cron, parsed by `croner`.
The cron uses the project `timezone` setting in the environment database.
An absent setting uses UTC. A setting with an invalid type or zone refuses that tick.

The environment document declares its applications: each name with the packages of the release that belong to it.
`environment::apply` refuses a release package that belongs to no application or to two, and a declared package that the release does not carry.
The release chart projects each application selection into a ConfigMap.
The host reads these release projections for each project in its environment.
The registry keeps each application's desired release and client selection in typed rows.
Helm keeps deployment attempts and rollback history for each application.
The host uses the declared application membership for each loaded release. A restored run retains its application and manifest digest.
A workflow belongs to one declared application, by name: `workflow author` and `workflow.start` refuse a name that the environment does not declare.
An application is a name, never a package. The workflow condition resolves each node within the application's packages first, then within any package of the release. A workflow run carries the application name as its package id: the queue leases the declared application names, and the executor resolves the run's nodes within that application's packages.

The host runs one schedule task per selected application. Each task wakes at a minute boundary.
Its first tick is strictly after startup.
At each tick it reads the enabled schedule rows of that application with one `SELECT`, and calls `wamn_workflow::admission::admit_schedule_run` for each row whose cron names that minute.
A host whose environment declares no application runs no schedule task.
An enable or a disable between ticks takes effect at the next tick.
The task polls the rows; it does not reload on an enable or disable message over NATS, which would cost one more subject and a stale window when the broker is lost.
The run has input `{"due":"<due RFC 3339>"}` and key `schedule:<application>/<workflow id>:<due RFC 3339>`, with UTC spelled as `Z`.
Replicas use the same key, so concurrent admissions return one run.
The run executes as the `workflow` user with its execution role, or with `admin` for type `invoker`.
Schedule reads and admissions use the runtime executor pool. Each admission reads the current release selection. A missing or unusable selection pauses admission for that application.
The due moment and its key remain UTC across daylight saving changes.

Apply creates one service principal per application that composes `wamn_integration`, in each environment.
Its subject is `wamn-app-<org>--<env>--<project>--<package id>`.
The project service row uses `<subject>@<platform-domain>` as its email.
Repeated applies retain its identity and grants.

Publish authorizes the operations that the application declares as dependencies.
Apply assigns a role with those permissions. Repeated applies write no principal or role rows.
The executor's database role reads only `id`, `type`, `status`, and `email` from `platform."user"`.
It can insert the principal into a run but cannot change that identity afterward.

The queue reads its current permissions, and the router authorizes each registered consumer command through the existing operation test.
A missing permission refuses that command.

Missed or refused ticks produce a log entry without catch-up, and host shutdown cancels the task.

The [JSONata node](../../apps/platform/execution/jsonata/src/lib.rs) is a palette node compiled to wasm.
Its parameter `expression` is JSONata text, and the node evaluates it on its input inside the guest.
The result leaves on port `main`, or on port `items`, whose schema is the item array that `label-render` takes.
The node is a std guest: it imports `wasi:random` at the version that Rust std links, and the build preserves that import.

The WMS workflow `movement_label` is the first example.
A `packaging` update starts it when the event condition `old.location_id != new.location_id` holds, so only a move starts it.
It stores one packaging label for each move, keyed by `{packaging_id}/{row_version}`, at the object path `wms/{key}`.

- A wiring is data with edges. Versioned. Swapped by version with no rebuild. `wamn-workflow` walks the catalog graph directly: frontier, ports, error edges, retry, hop budget, terminal verdict. Unchanged.

| Operation | Behavior |
| --- | --- |
| `list` | Returns the runs of one project and environment, newest first, with a limit. Each row gives the run id, the wiring, the trigger, the status, and whether the run is parked. |

The owner ruled on 2026-09-26 that the host admits event runs (`wamn-upl3.6`). The executor role gains a column-exact INSERT on `runs` and `run_queue` for the event run grain, and the run-plane grain SQL `CHECK` constraint gains an event arm. `list` shows the run, and `park` and `release` act on it.

### Environment workflows

An environment workflow is a wiring that an operator authors in one environment instead of in a package.
Its trigger is `manual`.
Each version is one row of `catalog.workflows` in the project database.
The row holds the application, the workflow id, the version, the graph, the wiring hash, the execution type, the execution role and `enabled`.

`wamn workflow author` evaluates the graph conditions against the release that the environment serves, and stores the version disabled.
The same bytes under the same version converge. Other bytes under that version refuse.
A stored version never changes except `enabled`, and at most one version of a workflow is enabled.

`enable` and `disable` take the lifecycle lock of the coordinate, as `environment::apply` does.
While another verb holds the lock, they refuse with `apply in progress`.
`enable` evaluates the graph conditions again, then disables the enabled version and enables this one in one transaction.
`disable` sets only `enabled` to false. Queued and running runs finish under the version they pinned.

The execution type decides who a run executes as.
A `role` workflow executes as the `workflow` user with its execution role.
The enabler must hold that role or `admin`, and the `workflow` user must hold that role.
An `invoker` workflow executes as the caller with the active role of the caller.
Every environment holds one `service` user named `workflow`, and the platform grants it no role.

The route `workflow.start` admits a run.
It reads the enabled version and evaluates the graph conditions against the release that the host serves.
It refuses `workflow_not_served` when the graph fails its conditions or its hash differs from the stored hash.
The active role of the caller is `admin` for a holder of `admin`, else the one role that the caller holds.
Any other caller refuses `active_role_required`.
The run records the release, the wiring hash, the component digest of each node, the principal and role it executes as, and the caller as its invoker.
The idempotency key is bound to the caller and the workflow.
A repeated start with the same key and input answers the first run, and another input refuses `idempotency_conflict`.
The run and its queue row commit in one transaction.

Before each node, the host reads the execution principal's current grants through its authentication memory.
The principal must be active and hold the run's selected role.
Only that role supplies permissions. The `admin` role grants administration.
A role revoked during a run refuses the next node. Leasing the run performs no caller read.

Every workflow is a graph run by the router and the run queue under a run row. A single-operation workflow is a one-node graph. The in-process walk with no row (`RegistrationDelivery::Walk`) is removed.

A workflow is keyed `(project_env_key, environment, application, workflow_id, version)`; a package version is not part of its identity. A version row is immutable (graph, hash, trigger, `execution_type`, `execution_role`); `enabled` is its one mutable field, and at most one version of a workflow is enabled at a time — a partial unique index on `(project_env_key, environment, application, workflow_id) WHERE enabled`; enabling a version disables the previous one in the same transaction. A start selects the enabled version. Disabling stops new starts only; queued and running runs finish under the version they pinned. Its graph names operations as `package.operation`; admission resolves them against the release the run pins. A package upgrade that keeps those operations does not touch the workflow; one that drops or changes them is refused at `environment::apply` by workflow and node.

Enable, disable and change of a workflow take the environment's lifecycle lock (`environment/lock.rs::LifecycleLock`, `pg_try_advisory_lock`, shared with `environment::apply`): while an apply holds it, enable is refused with "apply in progress" and the caller retries; the lock does not queue. So no workflow becomes enabled between the apply's test and its Helm write. Authority lifetime: the enabler's authority is validated at enable and change. An enabled workflow belongs to the environment, not to a session. Revoking the execution role's permission refuses the next protected call under the existing per-operation test. No author query on every start. Deploying logic that runs on every record change is an admin act; a project that wants a narrower publisher makes a role holding the enable operation.

- One invocation function serves both. The route calls it once. The router calls it per node.

3. A workflow calls application operations. It never touches a component, a table, or SQL directly. Same grant, intent, and never-replay rules as any client.

4. One invocation path. `invoke_operation(component, operation, input) -> Outcome`. Routes, workflow nodes, and the edge all call it.

5. Composition (`wac`) makes one component out of several at build time. A composed component is still one component behind one route. Composition and the router are unrelated.

6. Expression nodes (JSONata, templates, and so on) are platform node components compiled to wasm. The expression is config. It runs in the guest sandbox, never in the host.

7. The base platform and the edge do not link the router.

Every layer exposes an interface and depends only on layers below it: base platform, then run-state store, then workflow crate. Nothing depends on a UI.

- The WMS `inventory_move_and_label` graph runs as a registered workflow, off the request path.

- A workflow calls operations through `invoke_operation`, with the same grant, intent, and never-replay rules as any client.

- The base platform and the edge do not link the workflow crate. The dependency tests keep this rule.

Park acts on the queue row only. The run status stays `dispatched`, so the retired run-level `parked` status does not come back. A park inside a walk needs a saved walk and is part of approvals, which are out of target.

The node is a std guest at `apps/platform/execution/jsonata`. It exports `wamn:node/handler@0.1.0`, like `label-render`. Its one parameter, `expression`, holds the JSONata text. The node evaluates the expression on the input and emits the result. An input that is not JSON is an `invalid-input` error. An expression that does not parse, or an evaluation that fails, is a `terminal` error, because a retry gives the same result.

The `shape` node turns the row event into the item array that `label-render` takes, with `packaging_id` and `location_id`, and the label key `{packaging_id}/{row_version}`. Epic 25 (`docs/history/wms-inventory-spec.md`) moved the trigger from the movement insert to the packaging update. One move therefore stores one label, and a redelivered event overwrites it. The graph has no `respond` terminal, because no caller waits. The WMS cluster cases read the label from the blob store after the run completes. They no longer read it from the move response.

A PAT-authenticated service can start an invoker workflow with its own held role.
The host derives that role from current grants, with the existing single-role or held-admin selection rule.
A service never borrows a user's active role. User sessions with a service role or any unheld role still refuse.

An event trigger names a source package, entity, operations and an optional condition over the old and new images.
Provisioning declares one durable consumer per application source before hosts attach.
Workflow enablement prepares the same declaration before it records the stream tail as the enable boundary.
Workflow matches messages above that boundary and admits every matching run in one transaction.
The runtime plugin pulls messages and acknowledges them after the transaction commits.
Concurrent hosts share each application consumer. A redelivery retains the original run and release.


A package ships no workflows and no triggers; a custom operation's `registration` leaves `wamn.k`, and the operation stays. The two shipped today (WMS label, NetSuite pull) become environment data authored in their environments.

Packages ship only their own components. The platform build owns palette guests. Test workflow examples live under the application test directories; environment authorship owns the running workflows.

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
PAT authentication reads the current service principal and its credential.
Authorization reads the principal’s operation permissions in the target project database.
The host keeps both answers in its [auth memory](#auth-memory).
Service credentials use their authored application grants and cannot mint a user session.
The route authenticator requires neither the platform operator subject nor a system `admin` role.

A session token carries the user roles selected at minting and its revocable authority.
For each new request, the host makes sure of the principal, environment membership, and password login.
It intersects signed roles with the held roles and their permissions for the active project user.
A caller that holds `admin` holds every operation of the serving release. Any other role holds its stored stable references `<package>:<interface>/<operation>`, and the host matches each sealed operation of the release by its reference.
Missing, revoked, expired, or mismatched authority returns 401.
An unavailable identity read refuses admission with 503. The read has a five-second timeout.
Logout, reset, account disablement, membership removal, and PAT revocation reach an admitted PAT or session with the next drop of the [auth memory](#auth-memory).
A host that cannot receive drops caches nothing, so it reads every request.
The configured revocation bound applies when a drop is lost.
Already admitted work retains its caller. Revocation does not cancel that work or undo its effects.

Create an API identity with `wamn create-api-service`.
Supply the project, environment, name, display name, one or more `--role` values, and `--lifetime-seconds`.
The verb uses `WAMN_CONTROL_URL` and `WAMN_TOKEN`. The session selects the org.
The native service selects the owning database connections and PAT issuer credentials.
`--emit-secret` names a private JSON output file. The verb prints only the principal id.
It creates or reuses the service, grants authored roles in the selected environment, then requests its PAT from identity.
A partial creation can be retried. An API service cannot receive the reserved `admin` role through this verb.

### Auth memory

The host answers the auth facts of a repeated caller from its memory, so they cost no Postgres round-trip.
The [memory](../../crates/platform/capability-authority/src/auth_cache.rs) is the local replica of the NATS KV bucket `wamn-auth` on the one NATS.
The key `<project_env_key>.<principal>` holds the grants of a principal and whether the project has its user row.
The key `<project_env_key>.environment` holds the environment status.
One read of `platform."user"`, `platform.user_role` and `platform.permission` answers permission and user-row requests.
Invocation binds and workflow callers use the guest login. Route authentication uses the callable-HTTP login.
Both use the same grants reader and memory value.
A host that reads a fact puts it with the revision of the key from before the read, so a delete during the read refuses the put.
The environment host watches each project through that project's event client and keeps its connection facts separate. A fact read for one project never supplies another project's authority.
The control host serves every project, so it watches every key.
Each environment document declares the project's `revocation_seconds` and each application's `revocation_seconds`.
Both values are required nonnegative integers. Zero disables caching.
Release preparation writes these values as `revocation-seconds` rows in `platform.setting` before Helm starts the host.
Local development writes the same configuration before taking its database template.
The HTTP admitter reads these rows through its existing read authority.
The project row uses `target_type = project` and the project name as `target_id`.
Each application row uses `target_type = application` and its declared application name as `target_id`.
A host with authenticated routes refuses missing or invalid configuration at startup.
It uses the shortest bound across the project and its applications for shared auth facts.
Zero disables caching. A configuration change takes effect when the host restarts.
The auth bucket has no platform-wide expiry. It holds at most 1 MiB of facts.
A full bucket refuses a cache write, and the host keeps reading Postgres.
The replica measures each value from its original publication time.

Moka keeps admitted PATs and active sessions in the host that read them, with the configured expiry.
The host keeps only a positive identity answer, so a new PAT or a new login works on its first request.
A delete of the key `=identity.<principal>` drops the PATs and sessions of the principal on every host.
An `project_env_key` holds only `[A-Za-z0-9_-]`, so that key never meets an environment cache key.
A host does not keep a token answer when any identity key changed during its read.
A watch reset also invalidates reads already in progress.
Every cached token answer ends at its credential deadline, even when the configured bound is longer.
While the watch is broken, the host keeps no fact and opens the bucket again.
If the stream ends or the NATS client loses its connection, the watch breaks.
The host makes sure of the connection, because the client reconnects without ending the stream.
A host without the bucket keeps no fact either, and reads every request from Postgres.

Every function that writes one of these facts takes an [`AuthDrops`](../../crates/broker/src/auth_changes.rs) and records the change in it.
Its caller deletes the keys of the list after the commit. `wamn-broker::Publisher` publishes both authorization and connection changes through one client.
[`auth_drops_live`](../../crates/identity/platform/tests/auth_drops_live.rs) runs each writer once and makes sure of the drop it records.
A change of a user row or a user role deletes the key of the principal.
A change of a role, a permission or the environment status deletes every key of the project.
A PAT revoke, a logout, a reset, a replayed renewal, a membership revoke and a disabled principal delete the identity key of the principal.
The writers are the application host, the control host, the identity service and `wamn-ctl`.
The identity service and `wamn-ctl` read the connection from `WAMN_NATS_URL`, `WAMN_NATS_TLS_CA`, `WAMN_NATS_TLS_CERT` and `WAMN_NATS_TLS_KEY`, with a client certificate of the wasmCloud CA.
`WAMN_NATS_TLS_SERVER_NAME` names the server certificate when a port-forward reaches the server.
A lost delete waits for at most the configured bound.

The exact audience is `urn:wamn:project-env:{org}:{env}:{project}:{instance_suffix}`.
The configured environment supplies its current suffix.
Identity offers and mints no audience of an environment whose `registry.environments.status` is `inactive`, so the shell does not list it.

Session tokens use Ed25519 keys with `alg=EdDSA` and `typ=wamn-session+jwt`.
They require `iss`, `sub`, `org`, `aud`, `roles`, `exp`, `iat`, `jti`, and `authority`.
`authority` is the object `{"login":"<login UUID>"}`.
The optional `csrf` JWT claim is the lowercase SHA-256 hex of a CSRF token. Only the cookie carrier sets it.
The password issuer binds the existing login record.
The `kid` selects a key only within the configured issuer.
Wrong issuer, organization, audience, algorithm, signature, or time bounds returns 401.
Unknown roles grant no implicit authority, and an empty permission set returns 403.

A browser presents the session token in the `__Host-wamn-session` cookie instead of the `Authorization` header.
The host reads that cookie only when the request has no `Authorization` header and the route admits a session.
A request that carries both refuses with 401, and so does a repeated session cookie.
The host verifies a cookie token as it verifies a bearer token, and then applies the CSRF test.
A cookie token without the `csrf` JWT claim refuses with 401.
The CSRF test needs one `x-wamn-csrf` header whose SHA-256 hex equals the JWT claim.
A read route (type `get`, `query` or `projection`) needs no header, because a read changes nothing.
Every other route and every attachment to a wiring requires the header. The cookie carries the `csrf` JWT claim on every route.

The token lifetime is at most 900 seconds, with 30 seconds of time tolerance.
Minting anchors expiry to the start of credential validation and records the actual signing time in `iat`.
A delayed mint cannot restart the lifetime.
The host reconsiders admission freshness after permission reads.
Age bounds limit delayed admission without promising that identity remains unchanged during a pause.

Authentication deadlines bound new admission and do not cancel work already accepted.

The [session token owner](../../crates/identity/protocol/src/token.rs) defines the token and time rules.
The [session verifier](../../crates/identity/protocol/src/verifier.rs) takes its keys from a key source: the issuer over HTTPS in the cloud, or a key file on the edge.
The browser client holds no token. `@wamn/web-runtime` signs in with the cookie carrier and renews on load and before expiry.
Password sessions have no PAT fallback. Renewal happens only when an application request needs a new access token.
A local credential refusal occurs before HTTP submission and records a refused operation, not an unknown server outcome.
Failed renewal or login expiry requires explicit login and submission. Quitting clears local credentials and requests server logout.

### Connection memory

The host answers the connection facts of a call from its memory, so a repeated call reads Postgres and the credential store only on a miss.
The [memory](../../crates/platform/connections/src/connection_memory.rs) is the local replica of the NATS KV bucket `wamn-connection` on the NATS of the auth memory.
It follows the revision and watch rules of the [auth memory](#auth-memory).
Connection facts keep their own 60-second expiry.
The [shared replica](../../crates/platform/capability-authority/src/kv_memory.rs) holds these rules for both memories.
The key `<project_env_key>.connection.<connection id>` holds the active generation, effective status, credential id, and service principal id.
It also holds exclusivity, target id, resource key, and target type.
The effective status is active only when the integration and the connection are both active.
The key `<project_env_key>.release.<manifest digest hex>` holds the map of a release from (component digest, package, name) to (connection, version).
The key carries the hex part of the digest only, because a key token holds no `:`.
The key `<project_env_key>.connections` holds the map of the environment from integration name to connection id.
Each host watches the keys of its `project_env_key`, and the control host watches every key.

Immutable rows stay in the host under their own ids and never enter the bucket.
These rows are the parameters of a generation, the artifact and exports of a version, and the secret of a credential.
A secret never leaves the host.
A rotation makes a new credential id, so the delete of the connection key is enough to reach the new secret.
Inbound signature authentication resolves the credential name through the environment map.
The active connection fact supplies the credential id and service principal.
The host resolves the secret under that credential id.

The [engine](../../crates/platform/engine/src/integration.rs) compiles the component of a wasm or native version once per component digest and keeps it in host memory.
The [dispatch](../../crates/platform/integrations/src/lib.rs) links an instance per (connection id, generation, credential id) from that compiled component.
Typed integration imports use labels, the connection names that code declares.
The host resolves each label by its component digest, package, and name.
HTTP and blobstore share one authority resolver.
Their immutable component declarations, including connection modes, come from the loaded release.
The admitted invocation supplies the workflow position and node configuration.
Connection memory supplies current binding and credential state.
The host does not rebuild release or workflow authority from SQL on an effect call.

An exclusive connection acquires a resource lease, a lock held by its host, before its first call.
Each project has a dedicated system database session for its cloud leases.
The host reads connection configuration from that project's mounted directory under `/etc/wamn/environment`.
The session opens on the first exclusive cloud call.
Its key is `hashtextextended('resource:' || target_id || ':' || resource_key, 0)`.
Local targets use an in-process map with the same resource string.

The host serializes calls from every component bound to the connection.
A call waits up to 30 seconds for a lease, then returns its interface's `unavailable` error.
If the session drops, the host refuses calls until it reacquires the leases that its connections still need.
Readiness never waits for a resource lease.

A connection key delete stops new calls and releases its lease after the current call finishes.
During shutdown, the host stops the queue, drains exclusive calls, and closes the lease session before native cleanup.

Published versions record which exported interfaces permit writes in `write_interfaces`.
Environment apply refuses a read declaration that imports one of those interfaces.
Host types retain their method and storage enforcement at run time.

`connection.test` validates candidate values with the connection writer's parameter and credential schemas.
It uses the live release's bound version, or the newest published version when no live binding exists.
An explicit version overrides that default.
The host resolves metadata without creating a connection and rolls back its read transactions before network I/O.

HTTP opens TLS and sends one authenticated HEAD on the base path.
GCS and S3 list at most one object under the base prefix.
Wasm and native health use the digest cache and a fresh store with candidate values.
The route returns no credential and writes no generation or credential row.

A connection write or a credential change links again, but it never compiles again.
The dispatch keeps at most 1024 links, and it drops a link that no current fact names on the next fact change of that connection.

`connection.authorize` takes `project`, `env`, and `name` under ProjectAdmin authority and returns `authorization_url`.
The `oauth2_authorization_code` connection stores `client_id`, `authorize_url`, `token_url`, and `scopes` in `parameters.auth`.
Its credential contains `client_secret`.
`connection.get` returns `redirect_uri` for registration with the provider.
Calls return `unauthorized` until a callback stores the first token.
Candidate `connection.test` continues to refuse this authentication type.

The public control origin serves `GET /oauth/callback` without a session.
The control configuration supplies that origin from `route_hosts.control`, including during development on `kind`.
The host appends `/oauth/callback` to that origin.
`connection.authorize` stores 32 random bytes as base64url state and an encrypted PKCE verifier for ten minutes.
PKCE binds the authorization code to the verifier through a SHA-256 challenge.
The authorization URL preserves existing query parameters and adds the state, callback address, scopes, and S256 challenge.

The callback opens its own connection through the control login and holds no org write lock during the provider call.

It locks the state and active connection generation, deletes the state, exchanges the code, and writes the encrypted token in one transaction.
A successful callback consumes the state once.
Every refusal rolls back, including provider errors, expired states, and stale generations.
Expired or stale states remain until a generation write or connection deletion removes them.
A generation write deletes pending states, so a later callback reports an unknown state.
The callback escapes all page values and returns no credential material.

One OAuth module uses `oauth2` with its default HTTP client disabled.
It builds all grants and PKCE requests and validates bearer token characters.
The pinned transport sends each request once and follows no redirect.

Every OAuth type accepts an optional `parameters.auth.token_authority` as an exact `host:port` for HTTPS token requests.
Without it, token requests use the connection authority and its base path.
An explicit token authority permits a separate HTTPS origin with the same address tests and pinned transport.
The host refuses any other token origin before sending a request.
This applies to token fetches, refreshes, candidate health, and authorization code exchanges.
The browser's `authorize_url` stays outside this token boundary.

OAuth storage uses `identity.oauth_token` and `identity.oauth_authorization` in the system database.
Only the host connection role and the control role receive table grants.
Each row uses a fresh AES-256 data key and AES-256-GCM encryption.
An envelope key protects that data key.
The shared [encryption module](../../crates/platform/connections/src/oauth_encryption.rs) supports regional Cloud KMS and a local key file.
The host reads its provider from `WAMN_OAUTH_ENCRYPTION`.
Use `gcp` for Cloud KMS or `local` for a key file.
[Deployment instructions](../operations/gcp.md#49-platform-install-inputs) describe that configuration.
Each host retains at most 1024 unwrapped keys and evicts the least recently used key.
The cache identity includes both `key_id` and `wrapped_key`.
Debug output omits keys and plaintext.

The host refreshes tokens under a system database transaction.
It locks `registry.connection` with `FOR NO KEY UPDATE`, then reads the shared token row again.
A token is due when its expiry is within 60 seconds, including negative infinity.
A waiting host uses the row that the first host writes.
The lock also serializes refresh with credential rotation.
A stale generation cannot write a token.

The host keeps decrypted access tokens under the connection id and generation until the refresh margin.
A connection key deletion drops those entries.
An OAuth 401 follows the same lock path and permits one fresh token and one retry.
Concurrent refusals reuse a token that another host already refreshed.
A failed refresh returns `credential-unavailable` without another API request.

The ciphertext encodes a JSON object with `access_token` and `refresh_token` members.
A missing member value is JSON null.

Authorization code needs an existing row and uses the refresh-token grant for a due token.
A response without a replacement refresh token keeps the prior value.

A failed first fetch stores an encrypted object with both members null.
Failures set `refresh_status` to `failing` and record the database clock time.
All hosts refuse further attempts for 30 seconds.
After that window, the next call attempts refresh again.
A successful refresh sets the status to `ok`.
`connection.get` and the CLI show `refresh_status` and UTC `expires_at` without token bytes.

The generation transaction deletes tokens for either client credentials type when credentials or `parameters.auth` change.
For authorization code, a change to `parameters.auth` deletes the token.
A secret change alone sets its expiry to negative infinity and preserves its encrypted token bytes.
Every new generation deletes pending authorizations for that connection.
Connection deletion cascades to both OAuth tables.

Every writer of these facts records the change in a [`ConnectionDrops`](../../crates/broker/src/connection_changes.rs).
Its caller deletes the keys of the list after the commit. `wamn-broker::Publisher` publishes both authorization and connection changes through one client.
A failed delete is logged, the commit stands, and the 60 seconds bound it.
A connection write, copy, credential change, disable, enable or delete deletes the key of that connection.
A connection write, copy or delete also deletes the connection map of its environment, because it can create or remove a connection.
An integration delete deletes no key, because it refuses while the integration has a connection.
An integration disable or enable deletes the key of each of its connections, in every environment of the project.
An `environment::apply` deletes the release key of each release whose connection rows changed.
An `env delete` deletes every key of the environment.
[`connection_drops_live`](../../crates/identity/platform/tests/connection_drops_live.rs) runs each administration writer once and makes sure of the keys it deletes.

### Control sessions

Every org in `registry.orgs` has the control audience `urn:wamn:control:<org>`, and nothing configures it.
Discovery offers it to a user with an org, environment or project administration grant in that org, also when the request names a project, so an application shell offers it beside the environments.
A control session comes from a password login.
It carries no roles.
For each request, the control host reads the password login of the session, the active principal and its `org-admin` and `project-admin` roles in the org.
A revoked role therefore refuses the next request and the next renewal with 401.

### Consistent session access

Sessions and PATs use the same operation permission condition: the caller holds the entry.
Every user session receives the same current authority tests without operation-specific password prompts.
Renewal stays automatic during active use and never extends the absolute login deadline.

The identity reader needs SELECT on `identity.password_logins` in addition to its existing identity tables.
Session-only hosts also require their targeted `WAMN_SYSTEM_URL` identity reader.

Server authorization is authoritative. The UI may hide unavailable actions, but the router or host handler always enforces authority.

Every admin write runs in the host. `wamn_app` remains SELECT-only on:

```text
platform."user"
platform.role
platform.user_role
platform.permission
```

No guest component writes an authorization relation. No guest component receives a `wamn_system` connection.

`operator` is removed.

`permission.grant X` requires an existing authored role and an operation `X` that the current serving release serves. It inserts one row. The operation is idempotent.

`permission.revoke X` deletes that row.

No row is derived from another, so no revoke keeps or refuses a derived row.

The fixed application-administration operations are **not grantable** to authored roles.

`permission.grant` refuses them.

`admin` receives them because `admin` holds every operation the release serves.

`permission.mine` is the exception: it is readable by any authenticated application session and is not itself an authored permission.

`admin` is the only built-in application role.

The host refuses a session when any signed role is absent from the user's current grants.
It also refuses a service principal presented as a user session.
A service principal has no user active role, even when it holds an application permission.
Principal and grant writes record `AuthDrops`, and their callers publish the deletions after commit.
A failed deletion is logged without retry. The auth cache TTL bounds stale access.

A session with an unknown or unheld signed role, or a disabled principal, receives 401. A failure to read the database receives 503.

Every other role:

- is authored per project;
- starts empty;
- is identified by a role slug;
- contains stable operation references, not package-version-sealed operation ids;
- may contain several directly selected permissions;
- may be held alongside other roles.

A user may hold no application role.

For a user session:

- permission-row changes take effect on the next request;
- role revocation takes effect on the next request because current `user_role` still intersects the signed role names;
- role grant takes effect at renewal or new sign-in because the new role name is not in the existing token.

For a PAT caller, current roles are read per request, so grant and revoke both take effect on the next request.

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
Every login and renewal repeats the current membership, environment instance, project-user status, and assigned-role tests.
Credential rotation and access-token signing share one transaction after those tests.
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
The source is the client address forwarded by ingress in `X-Forwarded-For`.
The Rust web front normalizes the final address in the chain before proxying a password request.
Direct requests without that header use the TLS peer address.
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
It reads membership, environment status, the active user and roles before it signs the token.
Its response contains `sub`, `access_token`, `token_type`, `expires_at`, `renewal_token`, and `login_expires_at`.
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
With `"cookie"`, the reply body holds only `sub`, `expires_at`, and `login_expires_at`, and the tokens travel in three cookies.
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
The cursor key contains the type (control 0, application 1) and audience.
Its cursor binding identifies the authenticated principal and the exact optional org and project filters.
Invalid limits or cursors return HTTP 400 with `{"error":"password request refused"}`.

Discovery stops after enough authorized candidates establish the page and continuation.
Authorization can reject candidates, so a page can examine more candidates than its limit.
Every page authenticates again and uses the existing five-attempt login bucket. HTTP 429 retains `Retry-After`.

Discovery returns no session, PAT, application address, or database credential.
It shares the login throttle and request deadline with session issuance.
An unavailable authority read fails the whole request instead of returning an incomplete list.
Session issuance repeats the access tests after selection. A discovery result grants no access.

The issuer never supplies application addresses.

The development environment owns a separate identity process from startup until explicit teardown.
Application builds preserve that process, its identity database, and its signing keys.
This process ownership adds no external identity provider.

Unknown accounts and incorrect passwords receive the same unauthorized response.
These routes are enabled only when the service has a Resend key and sender.
The terminal serializes renewal and preserves the original absolute deadline with both wall-clock and monotonic expiry tests.
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
The identity service runs signing key rotation on a persisted schedule.
`keyRotationSeconds` in the identity chart sets the interval. Its configured value is 2,592,000 seconds, or 30 days.
The service calls the existing publish, activate and retire operations.
Replicas serialize rotation with an issuer lock. Restarting a replica does not restart the schedule.
An issuer without an active key requires explicit activation, including after removal of a compromised key.
The previous public key stays available for the longest token lifetime plus clock tolerance after its signing cutoff.

The [key cache](../../crates/platform/authentication/src/session_keys.rs) and [key owner](../../crates/identity/platform/src/session_keys.rs) define these bounds.

## Operator clients

Generated screens use the [client contract](../../crates/schema/generator/src/client_ir.rs), derived from admitted declarations.
Operation types come from the manifest.
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
An event workflow runs after the write's response and can write other tables, which the browser cannot see. A stored read of such a table stays fresh until its `max-age` ends, and its ETag then reads the change.
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
A filter change or reload starts a new traversal. Pending requests disable the page controls.
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
Its [deployment](../operations/deployment.md#platform-ui-files) owns the edge routing and the org-only configuration.

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

The generated web client prepares typed UUID, timestamp, and numeric input.
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

Labels come from a `label` member on operations and fields in `wamn.k`, optional, defaulting to the field path.

Generated operation components remain the primitive UI. Composite screens may be hand-authored over those generated operations; they do not bypass the operation contracts.

No new route. `wamn-control:environment/status@0.9.0` gains three additive answer members: `users_with_role`, `roles`, `memberships` (nullable `int32`). A failed count read returns null without failing status. Additive members do not bump 0.9.0.

## Process lifecycle

Host readiness requires a real native command-loop beat.
Each release has its own queue worker and explicit executor scope. Queue liveness follows durable polls and successful lease renewals.
No timer creates a synthetic beat.
An absent first beat fails through the normal silence budget.

The host drains readiness, stops new schedule and event admission, and starts native HTTP shutdown together.
The same admission signal refuses further HTTP requests on existing connections.
One shutdown signal stops HTTP admission. Every resident release then drains its pinned queue within the shared shutdown bound. Shared cleanup follows.
Ingress failure, probe failure, or command-task failure follows the failure cleanup path.
The process bounds plugin coordination, auxiliary task cleanup, telemetry flush, and final runtime shutdown.
Cleanup timeout remains an error and can leave plugin cleanup incomplete.
The HTTP-stop allowance bounds coordination and does not guarantee native completion.
Upstream ingress stop ends the accept loop without gracefully retiring existing HTTP or HTTPS connections.
It sends no stop-triggered HTTP/1 connection close or HTTP/2 GOAWAY.
The platform edge closes each upstream connection after its response, so the next request selects a current Service endpoint.
Connection drain can therefore consume the shutdown budget even after admitted responses finish.

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
A refused span reaches no layer, and the tracing registry stores nothing for it. Span call sites, WAMN and upstream, do not make sure of sampling.
An unsampled request therefore creates only its entry span.
A caller's remote trace context does not stop the entry span, so the sampler still makes the decision.
Sampled requests retain their span names, fields, levels, and parent links. Log events remain unchanged, but console lines in an unsampled trace name no span below the entry.
Without a valid context, startup and local tracing keep their existing behavior.
Native dispatch and spawned request tasks carry the entered entry span, so they read the same decision.
Operational logs use INFO. Hosts send request traces to the platform OpenTelemetry collector.

The [deployment guide](../operations/deployment.md) owns probe ports, grace periods, and rollout commands.
The [native alignment page](native-alignment.md#current-limits) retains the unresolved operator retry and liveness limits.

## Change feed

A generated `query` with history defaults to a change feed.
Its operation can opt out with `feed = false` ([wamn offline and sync](../plan/wamn-sync.md)).
Authored reads do not support feeds yet.
The feed is the `query` operation with its feed part, `feed: { cursor, limit }`, on the same GET route, so it has the list's grant and no permission row of its own.
Its value is `{ rows, next_cursor, complete }`. Each row carries `id`, `row_version` and `deleted`, and a row that is not deleted carries `data`.
The page of such a `query` also carries `cursor`, which starts the feed at the page's snapshot.
The cursor is opaque: the URL-safe base64 of the model name and a snapshot xmin, which [`feed.rs`](../../apps/platform/data/access/src/feed.rs) encodes.
The statement reads entries from the cursor transaction and uses one snapshot for xmin, xmax, and transaction visibility.
It reads rows below xmin first, then fills remaining page space with visible rows below xmax.
Rows follow transaction and id order, with one change per id.
A cut page below xmin continues after its last transaction and id.
Any returned row at or above xmin sets `next_cursor` to xmin without an id and `complete` to true.
These rows repeat on later calls until xmin advances.
Every list row and change carries the statement's snapshot xmin as `row_version`, from the same value that supplies the cursor.
A deletion carries that snapshot version too.
The int64 wire format uses a decimal string.
The generated client exposes it as `rowVersion`.
A declared revision column remains the write guard for operations that name it.
A feed needs no revision column.
If a model column is named `row_version`, the feeding list replaces that result member with the snapshot version.
The page cursor still uses the model column when the list sorts by it.
Other operations keep the model column.
The generated collection replaces a held row when the incoming snapshot version is equal or greater, and ignores a lower version.
The pinned `@wfe/sync` ignores equal versions.
The wamn adapter compares snapshot versions and gives accepted deliveries a local order through the package's version accessor.
The adapter keeps the snapshot value in each row and leaves `@wfe/sync` unchanged.
An empty page keeps its incoming cursor, so an unchanged feed still answers 304.
A cursor of another model, or one that does not decode, refuses with `invalid_input` on `cursor`. A cursor whose transaction is at or below the pruning watermark of the history table refuses with `cursor_expired`, because pruning removed a change that the cursor did not see.

### Device ingress

The device listens only on a loopback address. A non-loopback address refuses before startup.
The device has no request counter that sheds calls. Its instance pool retains warm instances only.
Calls beyond the warm pool can use one-shot stores.

### Environment placement

One native runtime serves every project of its org and environment.
The environment chart projects the allowed project names through `--projects`.
Each project has its own database connection configuration, connection reader, and event client certificate.
The host refuses a workload outside that project set before it loads components.
Project preparation retries independently. A failed project or release does not stop another project's queues.

Each application owns its execution objects even when applications use the same manifest digest.
Only verified manifest bytes share a cache. Every load first authorizes the digest against its own project catalog.
Current releases and releases pinned by unfinished runs stay loaded together.
Release preparation and queue execution remain independent.
Authorization reads remain live, so one project cannot set another project's cache lifetime.

Reset and deletion remove only the selected project from the host projection.
They wait for pods that mount that project's configuration to stop before replacing or removing its database.
Other projects remain in the shared host projection. The last project removes the host release.
Lifecycle exclusion uses org and environment. Application locks still include their project and application.
