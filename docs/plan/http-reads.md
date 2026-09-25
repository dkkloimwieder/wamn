# HTTP reads and caching

Epic 16 makes every generated read an HTTP GET with cache headers, and gives `web/runtime` a query cache. Beads epic `wamn-rst8` holds the issues and their status. The owner reviewed this scope on 2026-09-25.

## 1. Goal

1. The route kinds `get`, `query` and `projection` are GET. Every other kind stays POST. The request JSON does not change. A GET carries it in the query string, in one canonical encoding.
2. Each read response carries cache headers by kind. A `get` revalidates on every use, with an ETag from `row_version`. A `query` or `projection` can be reused for a short time, with an ETag from the versions of the models that it reads. A write changes those versions.
3. `web/runtime` gets a query cache. It sends one request for equal reads on one page. After a write, it reads the page again, and unchanged reads come back as `304 Not Modified`.

The owner decided two rules before this epic, and this page records them.

- Reads are GET and writes are POST. The method follows from the operation kind. Before this epic, three places wrote it as a literal: the authored attachments, the generator test routes, and the delivery check.
- A shared cache never serves one caller's read to another caller. The router checks the permission of each caller, and a shared cache does not run the router. So every authenticated read is `private`, and only a read whose auth policy is `none` can be `public`.

The catalog has no `list` kind. A list is a `query` or a `projection` whose result is a page or a bounded list, so "list" below means those two kinds.

## 2. Fixed rules

The owner set these rules in the epic brief.

- The contract, the operation SQL, the client IR field shapes, the plan and the components do not change. The route method, the request carrier, the response headers and the transport change.
- The method comes from the operation kind, at generation and at publish. No author writes it.
- An ETag comes from the record revision or from the model versions. It is never a hash of the body.
- A request with a CSRF header is never cached. A private response is never stored in a shared cache.
- The delivery check `method == "POST"` becomes a kind check.
- Every application that declares `client_package` regenerates in the same commit as the change that moves its bytes.
- The epic writes no edge proxy or CDN configuration. The headers must be correct for one to use later.
- The epic does not edit `web/ui`, `web/components` or the component emitter, because Epic 17 works there. It edits the transport and the cache in `web/runtime` only.

## 3. Current state

Measured on main at `07aa8d811` on 2026-09-25.

| Place | Today |
| --- | --- |
| `apps/*/publication/attachments.json` | Authors write `"method": "POST"` on all 48 routes. Nothing generates this file. |
| `crates/schema/generator/src/client_ir.rs` | Reads `route.method` from the authored file and refuses a method that is not uppercase. |
| `crates/control/lib/src/publish_release/attachments.rs` | Requires exactly a string `path` and `method`, and keys uniqueness on path and method. |
| `crates/control/lib/src/delivery/deployment.rs` | `require_released_route` requires `method == "POST"`, because the smoke interaction always sends a POST. |
| `crates/catalog/model/src/serving_manifest.rs` | `RouteKind::is_read()` is true for `get`, `query` and `projection`. Publish reads the kind from the generated contract. |
| `crates/platform/engine/src/flow_http_routing.rs` | `serves_read` uses that kind. Since `wamn-glgg`, a cookie read needs no `x-wamn-csrf` header. |
| `apps/platform/ingress/http-route` | Parses the body as JSON. An empty body is `null`. It sets no `Cache-Control`, `ETag` or `Vary` header. |
| Route input schemas | An array of 1 to 100 items. Each item requires `request_id`. |
| `web/runtime/src/transport.ts` | Always sends a JSON body. It has no cache and no dedupe. |
| Row revisions | Each Receiving and WMS table has `row_version int4`, and a get result marks it as the revision. No per-model version exists. |
| Authentication | Every application route admits `pat` and `session`. The `none` mode exists, but no application uses it. |

`wamn-zrrg` resolves a table reference with one get per record, so a table of 1000 rows can send 1000 gets. It is the first consumer of the query cache.

## 4. Decisions

Each decision lists the options and the owner ruling of 2026-09-25.

### 4.1 Query-string encoding

A GET carries exactly one request item. A client with several items sends several GETs, and the query cache makes equal ones into one request.

The brief fixes sorted keys, and arrays and nested members as JSON in one parameter. The open question is how a scalar is written.

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | Each top-level member is one parameter. Its value is the compact JSON text of the member, strings included: `?id=%22b1c2...%22`. | Quote characters in the URL. |
| B | Strings are raw text. The router reads the route input schema to decide how to parse each value. | The decoder depends on the schema, and a nullable string cannot tell `null` from `"null"`. |
| C | The whole item is one parameter: `?q={...}`. | One opaque parameter that a log or a person cannot read by member. |

Option A has one rule and needs no schema to decode. The encoder writes parameters in byte order of their names and escapes each value with the RFC 3986 unreserved set. The router refuses a repeated parameter, parameters out of order, and a URL longer than 8 KiB. Rust and TypeScript share one list of fixture vectors, so both encoders produce equal bytes.

### 4.2 Where the per-model version lives

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | A platform table `wamn_cache.model_versions (relation, version)` in each application database. A statement-level trigger on each model relation adds 1 to its row. apply-package installs the trigger, as it installs the record history trigger today. | Two writes to one model serialize from the trigger to the commit. |
| B | The generated write SQL adds 1 to the version. | Authored SQL writes do not update it, so the version can miss a write. |
| C | The host keeps the versions in memory. | Two hosts disagree, and a restart loses them. |
| D | The CDC reader derives versions from the change stream. | It adds a dependency on the change stream to every read. |

Option A landed with `wamn-rst8.3`, and [data access](../architecture/data-access.md#model-versions) describes it. By owner ruling of 2026-09-25, a statement trigger only records the changed relation, and one deferred trigger adds 1 to each recorded version in name order at commit. That removes the deadlock between two writers that change two models in opposite order, and it holds the version lock only through the commit.

Option A is correct for every writer, and the version becomes visible in the same commit as the data. A sequence or an append-only log avoids the lock. But it can show a version before its data, or miss a commit. The router reads the version before it runs the read. A newer response under an older tag only costs one extra read later. The opposite order can keep stale data under a current tag.

### 4.3 ETag strength

- A `get` has a strong ETag made from the release identity and the `row_version` of the record. A new release changes the tag, because it can change the response bytes.
- A `query` or a `projection` has a weak ETag made from the release identity and the versions of every relation that the operation reads. Publish knows those relations from the declared SQL access.
- A read that has no revision field, or whose relations are unknown, gets no ETag and `Cache-Control: no-cache`.
- For a `get`, the router runs the read and then compares the tag. The 304 saves the transfer. For a list, the router compares the tag before it runs the read, so the 304 also saves the database work.

These rules landed with `wamn-rst8.4`, and [execution](../architecture/execution.md) describes them. By owner ruling of 2026-09-25, the host compares the tag, not the router, and the router writes the status. The generator writes `relations` on every read contract, so the host reads them and never guesses.

### 4.4 `request_id` on a GET

Ruled: a read carries no `request_id` at all. A fixed value that the router adds is a `request_id` that means nothing, so the router does not add one. If the span of a read needs an identity, the router mints a trace identity, which is not a `request_id`. A write still sends its own `request_id` and `idempotency_key`.

Today `request_id` is a member of every generated operation contract and of the generated WIT item and outcome records, reads included. `wamn-rst8.1` measures what removing it from reads changes, and brings any WIT change to the owner first.

### 4.5 The query cache

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | A small store in `web/runtime`. It maps the canonical URL to an in-flight promise and a result. After a write, it marks every read as stale and reads the active ones again with `cache: "no-cache"`. | About 150 lines that the platform owns. |
| B | `@tanstack/query-core`, which is framework-free. | A new dependency with retry, garbage collection and focus refetch rules to configure. |

Option A is enough, because the browser HTTP cache stores the responses and the ETags make a fresh read cheap. The store only removes duplicate requests and re-reads after a write. It does not need to know which model a write touched: an unchanged model answers 304.

The store landed with `wamn-rst8.5`, and [execution](../architecture/execution.md) describes it. It keeps the replies itself and calls fetch with `cache: "no-store"`, so a 304 always reaches it. By owner ruling of 2026-09-25, any write marks every stored read stale. `wamn-fjdo` narrows this to the models that a write changes. Reading the active reads again after a write landed with `wamn-rst8.6`. It needs no fetch cache mode, because a stale stored read revalidates with its tag. A form's own record read keeps the revision that it read when it opened, so it does not read again.

### 4.6 Headers by kind

| Kind | Headers |
| --- | --- |
| `get` | `Cache-Control: private, no-cache`, strong `ETag` |
| `query`, `projection` | `Cache-Control: private, max-age=10, stale-while-revalidate=60`, weak `ETag` |
| Any write | `Cache-Control: no-store` |
| Any request with `x-wamn-csrf` | `Cache-Control: no-store` |
| Any refusal or failure | `Cache-Control: no-store` |

Every read response also sends `Vary: Authorization, Cookie`, so a browser does not give one user's cached read to the next user who signs in.

This table uses `private` for every generated read, as section 5 states.

The list values `max-age=10, stale-while-revalidate=60` are a provisional first pick. The owner revisits them with the CDN epic.

The `Cache-Control` and `Vary` rows landed with `wamn-rst8.2`, and the ETag columns with `wamn-rst8.4`. [Execution](../architecture/execution.md) describes them.

## 5. Shared caches

The brief says caller-dependent reads are `private`, and that the grant says which reads those are. Today no grant, route or operation declares that a result depends on the caller. Every read route requires authentication, and the router checks the caller's permission token on each request.

A shared cache that stores a `public` response gives it to the next request for the same URL. It does not run the router, so it does not check the permission of that caller. It also does not know the tenant. Every authenticated read is therefore caller-dependent in the sense that matters: the caller's grant decides whether the caller can see it.

Ruled on 2026-09-25: every authenticated read is `private`, and `public` applies only where the auth policy is `none`. No application route uses `none` today. This epic therefore delivers the browser cache, the ETags and the 304 responses.

A shared cache for authenticated reads is a separate decision for later. It needs a cache key that includes the grant, or a platform cache behind the permission check. That decision belongs to the CDN epic, item 7 of section 7 in [web operator client](web-operator-client.md).

## 6. Issues

| Bead | Issue | Depends on |
| --- | --- | --- |
| `wamn-rst8.1` | Read routes are GET. One function maps a kind to a method, and the generator, publish and delivery call it. Publish refuses an authored `method`, and all 48 attachments drop it. The router decodes the query string. The TypeScript transport and the Rust client encode it. Every `client_package` application regenerates. The route interface live test gives the same outcomes as before. | none |
| `wamn-rst8.2` | Cache headers by kind, as in section 4.6, with `Vary` and the CSRF rule. A fixture test covers each kind. | `.1` |
| `wamn-rst8.3` | The per-model version: the `wamn_cache` schema, the trigger, and apply-package installs it on each model relation. A live test shows that a write of each kind, authored SQL included, adds 1. | none |
| `wamn-rst8.4` | ETags and `304 Not Modified`: a strong tag for `get`, a weak tag for lists, the version read before the data. A router test gets a 304. | `.2`, `.3` |
| `wamn-rst8.5` | The `web/runtime` query cache: one request for equal reads, keyed by the canonical URL. Runtime tests cover it. | `.1` |
| `wamn-rst8.6` | Re-read after a write: a write marks the cached reads as stale, and the page reads its active reads again with `cache: "no-cache"`. Runtime tests cover it. | `.4`, `.5` |
| `wamn-rst8.7` | Closeout: move the behavior into `docs/architecture` and correct the stale line at `execution.md:210`. Run both applications in the demo with the cache, by hand. Run the full sweep, record the counts and the log path, and write the section 7 line. | `.1` to `.6` |

Issue 1 includes the router decode and both encoders. If no client can call a GET route, main fails its tests.

## 7. Done when

- Every generated read is GET and every write is POST. The route interface live test gives the same outcomes as before.
- A fixture test covers the headers of each kind, and a router test gets one 304.
- The demo runs Receiving and WMS with the cache, by hand.
- The closeout states the counts, the sweep log path and the section 7 line.

## 8. Out of scope

- Edge proxy and CDN configuration.
- A shared cache for authenticated reads (section 5).
- Streamed load reads from the table data loading proposal. A later streamed read can reuse the GET carrier.
- Changes under `web/ui`, `web/components` and the component emitter.
