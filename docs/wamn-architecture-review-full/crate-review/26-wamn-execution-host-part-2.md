# 26 wamn-execution-host, part 2: file routes and application host routes

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

All under `crates/execution/host/src/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `file_route.rs` | 1,688 | 1,108 |
| `application_route.rs` | 643 | 586 |
| `host_route.rs` | 442 | 442 |
| `environment_status.rs` | 365 | 213 |
| `file_collector.rs` | 104 | 104 |
| `oauth_callback.rs` | 86 | 74 |
| `credential_cleanup.rs` | 26 | 26 |
| Total | 3,354 | 2,553 |

Callers read: `services/host/src/host.rs` (file listener, OAuth callback route, host route setup, collector and cleanup tasks), `runtime/src/plugins/wamn_blobstore/plugin.rs::file_store` and `files.rs` (read in tab 19), `wamn_platform_identity::application` (signatures).

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `file_route.rs` | Native file upload, download, delete, expose, signing | Also runs its own HTTP server on its own port |
| `application_route.rs` | Application host routes (roles, grants, workflow start, counters) | One purpose |
| `host_route.rs` | Dispatch host routes, else pass to the next delivery | One purpose |
| `environment_status.rs` | `environment.status` for the control host | One purpose |
| `file_collector.rs` | Delete orphan file objects | One purpose |
| `oauth_callback.rs` | OAuth callback page | One purpose |
| `credential_cleanup.rs` | Destroy retired credential values | One purpose |

## Findings

### 1. File routes have their own HTTP server; the OAuth callback already uses the wash ingress

- **Fact.** `file_route::serve` binds its own TCP listener (`--files-addr`) and runs its own hyper server: connection set, keep-alive off, its own connection limit.
- **Fact.** `host.rs` serves the OAuth callback through the wash-runtime ingress: `on_service_http_resolved` with an `IngressRoute` (host, path prefix) and a `ServiceHttpJob` channel.
- **Fact.** `FileRoutes::http` already takes `hyper::Request<WasiBody>` and returns `hyper::Response<WasiBody>`, the types of a `ServiceHttpJob`.
- **Option (follows the owner direction on wasmCloud practice).** Register the file paths as ingress service routes, as the OAuth callback is. Delete `serve`, the `--files-addr` flag, the second listener and its task.
- **Open, to check before this is applied.**
  - The route host: an ingress route names a host; the release names none (R1). The http workload's `config.host` is the owner.
  - The path: `/api/<application>/files` needs one prefix per declared application, or one prefix `/api/`.
  - Whether the ingress limits a request body size. Uploads stream large bodies.

### 2. The file connection is checked twice per request

- **Fact.** `RegistryFileConnections::lookup` checks the active fact, the generation, the runtime type, the integration type and the version. `WamnBlobstore::file_store` then finds the same connection again by name and repeats every check (carried from tab 19).
- **Fact.** The doc of `file_store` says an existing file must use the row's generation. The code uses the active generation, and the file row stores no generation.
- **Option.** `file_store` takes the resolved `FileConnection` (id, generation, definition, read-only). Its own lookup and checks go. Correct the doc to what the code does.

### 3. Signing re-checks what the call already checked

- **Fact.** `FileRoutes::signed_url` reads the invocation from the blob store's per-scope map, finds the served component in the manifest again, and checks `caller.permits` on the registered operation again. The call was admitted with that same check.
- **Fact.** Tab 25 finding 2 (B1) moves signing behind `sign-result`, called while an operation is open on `wamn:operation`. Tab 25 finding 1 replaces the per-scope maps with one registry.
- **Option.** `signed_url` takes the authorized operation (package, operation, caller) from the open operation. The second manifest search and the second grant check go.

### 4. Two more database connections outside the pools

- **Fact.** `FileRoutes::lock` opens a new connection with the administration URL for every delete and expose, checks on each call that the login is a non-superuser member of `wamn_administration`, and takes a session advisory lock.
- **Fact.** `oauth_callback::response` opens a new connection with the control URL for each callback. The control host already holds a control writer connection.
- **Option A (recommended).** The role check runs once at host start. The per-mutation lock connection stays: the lock must span the storage call and the catalog write. It joins the "connections outside pools" list as a justified case. The callback uses the control writer connection.
- **Option B.** The lock uses a pooled administration connection, unlocks before return, and destroys the connection when the unlock is not confirmed.

### 5. A grant change empties the host's whole auth memory

- **Fact.** After a role or grant change, `ApplicationRoutes::handle` calls `auth_cache().clear()` and then publishes the changed keys. Every principal on the host then reads Postgres again.
- **Option.** Drop only the changed keys on this host, then publish. The repository rule that memory comes before CPU does not apply against this: the clear costs reads, not memory.

### 6. The set of served operation references is built twice

- **Fact.** `host_route.rs::served_references` and `application_route.rs::permission_list` each build "registered operations of the release plus host route references" from the manifest.
- **Option.** One function, part of the route projection built once (tab 21 finding 2, tab 25 finding 6).

### 7. A third parser of the request element

- **Fact.** `host_route.rs::request_element` reads the one request element of a host route (`request_id`, `value`) by hand. `engine/src/operation/intent.rs` and the guest `http-route` each have their own.
- **Option.** One parser in `wamn-execution-contract`. This is the carried request-element item.

### 8. A trait that exists for one test

- **Fact.** `FilePublication` has one production implementer (`FileStore`) and one test implementer. It exists so `change_exposure` can be tested without storage.
- **Fact.** The blob store tests already run `FileStore` over `object_store::memory::InMemory`.
- **Option.** Delete the trait. `change_exposure` takes `&FileStore`. The test uses the in-memory store.

### 9. Smaller items

- **Fact.** `start_workflow` turns an admission error into `anyhow!(error.to_string())`, which drops its source chain.
- **Fact.** `KubernetesSource::read` builds the Kubernetes client on every status read.
- **Fact.** `file_collector::namespace` reads `provider`, `endpoint`, `container`, `prefix` by hand: one more hand parser of the definition (tab 19 finding 5).
- **Option.** Keep the error as its source. Build the client once. The collector uses the one typed definition.

## Fine as is

- `put`: stage the bytes, write the row, then promote; a failed promotion is repaired on the next read.
- `expose`: a rolled-back catalog write removes the new public copy.
- `environment_status.rs`: one trait, two readers (cluster and local), one output shape.
- `oauth_callback.rs`: the page escapes provider text and sets no-store and a strict CSP.
- `credential_cleanup.rs`: 26 lines.

## Tests

**Delete**

- None in this part.

**Change**

- `file_route.rs::native_listener_uses_shared_authentication_and_preserves_file_paths`: through the ingress route (finding 1)
- `file_route.rs::writes_require_the_declared_application_binding_and_connection`, `missing_bindings_refuse_before_connection_lookup`: `file_store` takes the connection (finding 2)
- `file_route.rs::signed_urls_refuse_without_a_bound_operation_before_catalog_access`, `signed_urls_refuse_an_unregistered_export_before_catalog_access`: authority from the open operation (finding 3)
- `file_route.rs::exposure_transitions_retain_public_urls_and_repeat_origin_deletion`: in-memory store, no fake (finding 8)
- `tests/files_live.rs` (1,405 lines): setup for findings 1 and 2. Not read.

**Move**

- None in this part.

## Not read

- `tests/files_live.rs`, `tests/host_route_live.rs`: not read.
- Test bodies: names only.
- `wamn_platform_identity::application`: signatures only.
- wash-runtime ingress service routes: the call in `host.rs` only. The three open checks of finding 1 need its source.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1 to 3, 5 to 9: accepted.
- Finding 4: **option A.** Role check once at start; the per-mutation lock connection stays as a justified case; the OAuth callback uses the control writer connection.
- Finding 1, confirmed with the owner: the second HTTP server is removed. Goes: `file_route::serve`, its TCP listener, the `--files-addr` flag, the `file_tasks` set in `host.rs`. Stays: `FileRoutes::http` and the handlers, `PermitBody`. The file paths are registered as ingress service routes and their requests arrive on a channel, as the OAuth callback does. The three open checks are done first; if one fails, it comes back with options.
- Tab 26 is fully ruled.

## Check results, finding 1 (2026-11-11)

Source: wash-runtime v2.11.0 `host/http.rs`; wamn `services/host/src/host.rs`, `engine/src/expected_router.rs`.

- **Path prefix.** Fact: an `IngressRoute` has public fields; wamn already registers one with `RouteScope::Ingress` and a path prefix (`/oauth/callback`). The router keeps each host's prefixes longest first and takes the first match. The application workload registers an empty prefix (every path). So a platform prefix on the same host wins for its paths. Resolved.
  - Use one prefix per application, `/api/<application>/files`. Fact: application routes live under the same `/api/<application>/` prefix, so a single `/api/` prefix would take every application route.
  - New item: no manifest rule reserves the model name `files` (grep). A model named `files` would collide with the file routes. A: manifest validation refuses it.
- **Route host.** Fact: `Ingress::on_service_http_resolved` may be called again to re-register (supported restart path). Resolved: when the application http workload binds, the host registers the platform routes (`/wamn_control/`, `/api/<application>/files`, `/oauth/callback`) under that workload's `config.host`, the one owner (R2). Fact: today the OAuth callback takes its host from the redirect URI, a second owner of the route host. It moves to the same rule.
- **Body limit.** Fact: the ingress sets no request body limit; it bounds connections only. The service handler receives a streaming body. Resolved: each native handler sets its own limit (`PermitBody`, or `http_body_util::Limited`). Uploads stream as today.

## Ruling (2026-11-11): platform model names are reserved

- An application may not declare a model whose name is a platform model name. Manifest validation refuses it.
- The list is not written by hand. It is read from the platform's own model set, the host route model directories in `wamn-catalog` (`host_route/contracts/`, `host_route/control/contracts/`). Today that is: `file`, `number_counter`, `permission`, `role`, `user`, `user_role`, `workflow` (application set); `connection`, `control`, `environment`, `external_system`, `integration`, `integration_type`, `member`, `org_admin`, `project`, `project_admin`, `release`, `user` (control set). A new platform model is reserved by being added.
- Fact: no such rule exists today (grep of manifest validation; the only reserved names are two control tables and the history suffix).
- Tests: one manifest test that derives the reserved set from the catalog and refuses each name (test rule 2: no pinned roster).

Still open: where the native file byte routes live. A: under the platform model's own path, `/wamn_control/file/...`, one URL space for `file` (recommended). B: keep `/api/<application>/files` and also refuse the model name `files`.

Fact (2026-11-11, owner question): no inflection function exists in the repository. `files` is a hand-written literal in `file_route.rs::native_route` (and its doc comments). `validate_identifier` says "must be singular snake\_case" but checks only characters, so plurality is not checked anywhere. Checking it by machine needs an English inflection rule, which is unreliable (`status`, `address`, `data`, `series`). Whichever path is chosen, the literal becomes the model name `file`.

## Ruling (2026-11-11): file byte routes and model names

- File byte routes: B. They stay in the application's space, renamed to the model's own name: `/api/<application>/file` (upload) and `/api/<application>/file/<id>` (download). The literal `files` in `file_route.rs::native_route` and its doc comments goes. The reserved platform names rule prevents an app model `file`, so there is no collision. This replaces the `/api/<application>/files` path in the check results above.
- Model names: singular is a convention, not a check.
  - Platform development: platform models are always singular.
  - Application developers may name models as they like; singular is the recommended practice.
  - Follow-on: `validate_identifier` says "must be singular snake\_case" but checks only characters. Its message drops "singular" (the same wording also appears, written separately, in `data_access.rs` and `package_migrations.rs`).
