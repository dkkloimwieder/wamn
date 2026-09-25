# Web deployment

Epic 20 serves the generated web clients from one public host: static files from a bucket behind a CDN, and an edge proxy that sends API paths to the platform. It is item 7 of section 7 in the [web operator client](web-operator-client.md) plan. Beads epic `wamn-xyxj` holds the issues and their status. This scope waits for the owner review.

## 1. Goal

1. A production build of a web application is one set of static files, and a command puts it in a bucket.
2. One public HTTPS host serves the page, `/password` and `/api` of one application. The edge proxy sends `/password` to the identity service and `/api` to the route ingress, and the bucket serves everything else.
3. A CDN caches the static files. It caches no authenticated read.
4. The first real deployment runs Receiving through that host, and a person signs in and completes a supplier change in a browser.

## 2. Fixed rules

These rules come from decisions that are already made.

- The page, `/password` and `/api` share one origin. The session and CSRF cookies are `__Host-` cookies with `Secure`, `SameSite=Strict`, `Path=/` and no `Domain`, so a second host cannot receive them. The client needs no CORS.
- The client calls `/api` and `/password` on its own origin, as it does in development today. The edge proxy removes `/api`, as the Vite development proxy does.
- The router selects a release by the `Host` header. The release publishes with `--route-host` set to the public host, so the edge proxy passes the host unchanged.
- Every authenticated read is `private`, and a shared cache never serves one caller's read to another caller. Only a route whose auth policy is `none` can be `public`.
- An application never learns a deployment fact at build time. The same static files serve every environment.

## 3. Current state

Measured on main at `f73453046` on 2026-09-25.

| Place | Today |
| --- | --- |
| `web/shell/vite.ts:57-79` | The development proxy sends `/password` to the issuer and `/api` to `WAMN_ROUTE_URL` with the route host, and removes `/api`. It runs only under `vite serve`. |
| `web/shell/src/shell.tsx:64` | `API_BASE = "/api"`. The session calls use the page origin. |
| `apps/*/web` | `pnpm run build` writes `dist/`. Nothing serves it, and the Dockerfile does not build it. |
| `services/identity/src/password.rs:40-42, 674-697` | Sets the three cookies. The identity service terminates its own TLS on port 443. |
| `deploy/platform/http-route-workload.example.yaml` | The `flow-http` Service is ClusterIP port 80, plain HTTP. |
| `deploy/` | No Ingress, Gateway or LoadBalancer. No public TLS. No cloud configuration. The wasmCloud gateway is disabled. |
| `crates/platform/engine/src/flow_http_routing.rs:801-819` | A get is `no-cache`. A list is `max-age=10, stale-while-revalidate=60`. The scope is `private` unless the auth policy is `none`. |
| Receiving, WMS | Every route admits `pat` and `session`. No route uses `none`. |

## 4. Decisions

Each decision lists the options and a recommendation. The owner rules on each one.

### 4.1 Where the edge runs

| Option | Shape | Cost |
| --- | --- | --- |
| A | A cloud load balancer. On GCP, a URL map sends `/api/*` and `/password/*` to the cluster and the rest to a backend bucket with Cloud CDN. | A cloud project, a domain and a certificate. It cannot run in kind. |
| B | One edge proxy in the cluster serves the files and forwards the two paths. A CDN can sit in front later. | The platform owns one more deployment. |
| C (recommended) | B first, tested in kind with MinIO as the bucket. Then A for the first real deployment, with the same path rules. | Two configurations of one rule set. |

Option C gives a tested shape before any cloud cost. The GCP shape is assumed and not tested until the real deployment.

### 4.2 One host per application or one host for all

| Option | Shape | Cost |
| --- | --- | --- |
| A (recommended) | One public host per application, for example `receiving.<domain>`. | One certificate name per application. |
| B | One host, with each application under a path. | The cookies have `Path=/`, so every application on the host receives every session. The router also selects a release by host, not by path. |

### 4.3 A shared cache for authenticated reads

| Option | Shape | Cost |
| --- | --- | --- |
| A (recommended) | None. The CDN caches static files and `public` reads only. | Each browser keeps its own read cache, as today. |
| B | A cache key that includes the grant of the caller. | A CDN cannot read a grant from a cookie, so the edge needs code. |
| C | A platform cache behind the permission check. | A new cache in the host. |

Option A needs no new code. B or C starts only after a measurement shows the need.

### 4.4 The list cache values

Keep `max-age=10, stale-while-revalidate=60`, because no shared cache stores an authenticated list under option 4.3 A. Revisit them with a measurement from the real deployment.

### 4.5 Static file headers

The files under `assets/` carry a content hash in their names, so they are `public, max-age=31536000, immutable`. `index.html` is `no-cache`, so a new release reaches the browser at the next load.

## 5. Owner input

The real deployment needs a cloud project, a domain for the public hosts, and the way to issue their certificate. The epic cannot supply them.

## 6. Issues

1. The edge in kind: one HTTPS host serves the built Receiving files from MinIO, and forwards `/password` and `/api`. A browser signs in and completes a supplier change through it.

The owner review decides the later issues: the bucket upload command, the GCP configuration and the first real deployment, and the closeout.

## 7. Out

- A shared cache for authenticated reads, unless 4.3 rules otherwise.
- The platform admin UI and its host.
- OIDC, SSR and live updates.
