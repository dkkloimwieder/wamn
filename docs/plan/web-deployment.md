# Web deployment

Epic 20, Beads `wamn-xyxj`, served the generated web clients from one public host: static files from a bucket, and an edge proxy that sends API paths to the platform. It was item 7 of section 7 in the [web operator client](web-operator-client.md) plan, and it closed on 2026-09-25.
The built parts are in the operations pages.
[Deployment](../operations/deployment.md#web-client-files) describes `wamn web upload` and the edge chart in `deploy/platform/edge`, including its [Google Cloud](../operations/deployment.md#google-cloud-edge) rendering.
[Cluster tests](../operations/cluster-tests.md) describes the kind edge case.

## 1. Remaining work

The first real deployment runs Receiving through the public host on Google Cloud, and a person signs in and completes a supplier change in a browser.
`wamn-ghx2` holds it. It waits until the owner names the cloud project, the domain and the certificate issuer.
The Google Cloud part of the edge chart is assumed and not tested. Only a rendering checks it.

## 2. Fixed rules

These rules come from decisions that are already made.

- The page, `/password` and `/api` share one origin. The session and CSRF cookies are `__Host-` cookies with `Secure`, `SameSite=Strict`, `Path=/` and no `Domain`, so a second host cannot receive them. The client needs no CORS.
- The client calls `/api` and `/password` on its own origin, as it does in development today. The edge proxy removes `/api`, as the Vite development proxy does.
- The router selects a release by the `Host` header. The release publishes with `--route-host` set to the public host, so the edge proxy passes the host unchanged.
- Every authenticated read is `private`, and a shared cache never serves one caller's read to another caller. Only a route whose auth policy is `none` can be `public`.
- An application never learns a deployment fact at build time. The same static files serve every environment.

## 3. Decisions

Each decision lists the options and the owner ruling of 2026-09-25.

### 3.1 Where the edge runs

| Option | Shape | Cost |
| --- | --- | --- |
| A | A cloud load balancer. On GCP, a URL map sends `/api/*` and `/password/*` to the cluster and the rest to a backend bucket with Cloud CDN. | A cloud project, a domain and a certificate. It cannot run in kind. |
| B | One edge proxy in the cluster serves the files and forwards the two paths. A CDN can sit in front later. | The platform owns one more deployment. |
| C (ruled) | B first, tested in kind with MinIO as the bucket. Then A for the first real deployment, with the same path rules. | Two configurations of one rule set. |

Option C gives a tested shape before any cloud cost. The GCP shape is assumed and not tested until the real deployment.

### 3.2 One host per application or one host for all

| Option | Shape | Cost |
| --- | --- | --- |
| A | One public host per application, for example `receiving.<domain>`. | One certificate name and one cookie scope per application. |
| B (ruled) | One public host for every application. | One certificate and one cookie scope. |

The shell already routes under one origin: `/api` for the platform and the environment as the first page segment. A second host is a later choice, when a tenant needs one. Receiving and WMS share no route path today.

### 3.3 A shared cache for authenticated reads

| Option | Shape | Cost |
| --- | --- | --- |
| A (ruled) | None. The CDN caches static files and `public` reads only. | Each browser keeps its own read cache, as today. |
| B | A cache key that includes the grant of the caller. | A CDN cannot read a grant from a cookie, so the edge needs code. |
| C | A platform cache behind the permission check. | A new cache in the host. |

Option A needs no new code. B or C starts only after a measurement shows the need.

### 3.4 The list cache values

Ruled: keep `max-age=10, stale-while-revalidate=60`, because no shared cache stores an authenticated list under option 3.3 A. Revisit them with a measurement from the real deployment.

### 3.5 Static file headers

Ruled: the files under `assets/` carry a content hash in their names, so they are `public, max-age=31536000, immutable`. `index.html` is `no-cache`, so a new release reaches the browser at the next load.

## 4. Out

- A shared cache for authenticated reads.
- A second public host.
- The platform admin UI and its host.
- OIDC, SSR and live updates.
