# deploy/ — tiered by lifecycle (SR8, findings §1.6)

Four tiers. A new file goes in exactly one tier; nothing lands at
the top level. When in doubt, ask which lifecycle owns the file's create/delete.
`mvp/` is the classified exception: its bootstrap scripts remain outside the SR8
lifecycle tiers because pre-tier provisioning runs before any tier exists.

- **`infra/`** — install-once cluster infrastructure, applied by hand at cluster
  standup and rarely touched: operators (CNPG, barman plugin, cert-manager), the
  data-plane NATS, development observability inputs (Tempo/otel/MinIO), kind
  config, the cluster-scoped `wasmcloud-ca` ClusterIssuer
  (`wasmcloud-ca-issuer.yaml`), and the runtime-operator's own Helm values
  (`values-wamn.yaml`).
- **`platform/`** — long-lived production/platform manifests the control plane
  or an operator owns: dispatcher, the component+wiring router executor,
  registry, wamn-sysdb, credential `*.example` Secrets, the shared postgres
  fixture, and the per-environment runtime-operator host-tier Helm values
  (`values-host-*.yaml`) with their host client certificates
  (`host-environment-certs*.yaml`).
- **`gates/`** — gate/bench Job manifests (`*-job.yaml`) and their support
  Deployments (`serve-echo`, `egress-escape`). Applied per gate run, deleted
  after.
- **`sql/`** — the standalone SQL schemas (`postgres-init`, `app-schema`,
  `catalog-schema`, `system-schema`, `run-queue`, `run-state`).
  Several are `include_str!`'d or read by tests — paths are load-bearing
  (SR13 tracks generating these from Rust instead of hand-maintaining them).

Placement judgment calls, recorded: `postgres.yaml` is platform (the shared
long-lived fixture ~8 gates and the dispatcher point at, despite its bench
header); `serve-echo` is gates (gate support, not product).

Ordering recorded, **reconcile the run plane before publishing a release into
it** (wamn-0h0g.8.26). `wamn-ctl reconcile-run-plane` — the per-project-env Job
in `platform/run-plane-reconcile.example.yaml` — converges one tenant's row in
`<--schema>.environment_policies` out of the control registry. `wamn-ctl
publish-release --run-schema <schema>` reads `expected_environment` from that
same row before it commits, and refuses when the row is ABSENT
(`environment-policy-not-converged`) as well as when it names another
environment than the release carries
(`environment-policy-environment-mismatch`). Publishing into a project database
whose run plane was never reconciled therefore fails; the absent refusal names
`reconcile-run-plane` so the remedy is in the error text. Nothing sequences the
two verbs, which is why the ordering is written down here.
`push-release-manifest` has no such precondition — it publishes bytes a mint
already verified.

Release rollout, and rollback by revert: **the controller question re-opens on a
named trigger, and on nothing else.** Two fire it, and only two — a SECOND
ENVIRONMENT, or the first convergence with no human present: the platform half
of the two-speed model (packaged artifacts carried over OCI and Helm, as against
wirings, which are gated tenant rows activated by pointer flip) arriving as a
path something other than a person has to apply. Neither has happened yet, so no
bead carries a GitOps controller and none should be filed speculatively; the two
triggers are the thing to watch, and whoever trips one files it then. GitOps is
a deferred decision here, not a dead one. Until a
trigger fires there is no reconciler in this tree — no `Kustomization`,
`HelmRelease`, `GitRepository`, `OCIRepository` or `ApplicationSet` in any
manifest — and an operator with `kubectl` and `helm` is the shipped convergence
mechanism for all four tiers, not an interim stand-in. wamn-0h0g.15.16 closed
REFUTED-AND-SPLIT on exactly that measurement, and this section is the residual
it split off.

**What `git revert` actually rolls back, and why that is enough.** No image any
manifest under `deploy/` schedules is digest-pinned — every one is a mutable tag
(`wamn-executor:dev`, `wamn-host:dev`, `postgres:18`, `registry:2`,
`quay.io/jetstack/cert-manager-*:v1.21.0`, and the rest), so reverting a commit
that changed one moves a *name* and guarantees nothing about the bytes behind
it. The single digest under `deploy/` is a Dockerfile `FROM` base image in
`gates/m1-postgres.Dockerfile`, a build input no cluster object reads. **The
release manifest digest in the two git-tracked pod templates is therefore the
only thing a revert genuinely rolls back**, and it rolls back to exact bytes:
each pod re-derives the digest of what it pulled and refuses to start unless it
equals the digest its template carried, so a reverted digest cannot resolve to
anything but the release it named before.

**The two carriers, and why they are not the same shape.** Both hold a 64-zero
placeholder as checked in, and a digest that addresses no artifact fails the
pull, so both crashloop until a real release is written in.

- `platform/executor.yaml` — an env PAIR, `WAMN_RELEASE_ARTIFACT_BASE` and
  `WAMN_RELEASE_MANIFEST_DIGEST`, on the Deployment's container.
- `platform/values-host-default.yaml` — an extraArgs FLAG pair,
  `--release-artifact-base=` and `--release-manifest-digest=`, under
  `runtime.hostGroups[].extraArgs`.

The host takes flags DELIBERATELY, and the reason is in the two binaries' arg
shapes rather than in taste. **The host's pair is OPTIONAL** — both absent is a
legitimate state meaning "this host serves no release" — so a misspelt
`WAMN_RELEASE_MANIFEST_DIGES` would leave the pair absent, deploy cleanly, and
serve nothing, indistinguishable from the state someone chose on purpose. Carried
as a flag instead, clap exits nonzero on an unknown argument and the Pod
crashloops: loud, and it stalls the rollout. **The executor's pair is REQUIRED**,
so a value that fails to arrive is refused by the parser whichever carrier it
travels in, and the executor can afford the plainer one. (Half a pair is refused
in both binaries; the flags-versus-env choice only decides what happens when a
typo loses *both*.) The host tier is also rendered by a chart with no release
model of its own, which is why its pair rides `extraArgs` rather than a chart
value; the executor's Deployment is hand-written.

**Rollout.** Reconcile the run plane first (above), then:

```bash
# 1. MINT — freezes release identity and prints THE DIGEST AS THE WHOLE OF
#    STDOUT. The deployment-attestation coordinate is a tracing record and goes
#    to stderr, so `$(...)` around this captures the digest and nothing else.
wamn-ctl publish-release \
  --database-url "$OWNER_URL" --org "$ORG" --project "$PROJECT" \
  --tenant "$TENANT" --catalog-id "$CATALOG_ID" --catalog-version "$CATALOG_VERSION" \
  --run-schema "$RUN_SCHEMA" \
  --wiring "$WIRING_ID=$WIRING_VERSION" \
  --attachments attachments.json --registrations registrations.json

# 2. PUSH the frozen bytes as an OCI artifact, read back from the snapshot the
#    mint wrote rather than from a file, and re-print the same digest.
wamn-ctl push-release-manifest \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --catalog-id "$CATALOG_ID" --catalog-version "$CATALOG_VERSION" \
  --org "$ORG" --project "$PROJECT" \
  --artifact-base "$PUSH_BASE" --registry-auth-file "$PUSH_DOCKERCONFIG"

# 3. READ the six lines the templates take — both carriers, each labelled with
#    the file it belongs in (wamn-duyl). --artifact-base here is the base THE
#    PODS read, which is not necessarily the one step 2 pushed to.
wamn-ctl print-release-env \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --catalog-id "$CATALOG_ID" --catalog-version "$CATALOG_VERSION" \
  --artifact-base registry.wamn-system.svc.cluster.local:5000/wamn/releases

# 4. HAND-EDIT both files with those lines, in one commit. There is no verb that
#    writes them; see the ruling below.

# 5. APPLY. No ordering between these two is recorded anywhere in deploy/, and
#    each refuses to serve a release it cannot verify, so either order is fine.
kubectl -n wamn-system apply -f deploy/platform/executor.yaml
kubectl -n wamn-system rollout status deploy/executor --timeout=300s
helm upgrade --install -n wamn-system wamn-host \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.8.0 \
  -f deploy/platform/values-host-default.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=150s
```

`$PUSH_BASE` and the base written into the templates name the same repository
reached by two different authorities: a publisher outside the cluster pushes
through a port-forward to `localhost:5000/wamn/releases`, while pods pull
`registry.wamn-system.svc.cluster.local:5000/wamn/releases`. The registry
addresses artifacts by repo path, hostname-independent, so the same path
resolves to the same manifest and blobs from either side — `platform/registry.yaml`
carries that fact and the port-forward push flow that depends on it. The repo
path is what must match; the authority need not.

`executor.yaml` runs `maxUnavailable: 0`, so a bad digest, a missing binding or
an unreachable registry stalls the rollout at 503 instead of replacing a serving
replica. That is the safety property the sequence leans on: step 5 is not a
commit point, and an unhealthy release never displaces the one running.

**Rollback is `git revert` plus re-apply.** Revert the commit from step 4 and run
step 5 again. Nothing else in the release path is stateful in a way a revert
misses: the mint is append-only and the pushed artifact is immutable, so the
previous digest still addresses the previous bytes, and there is nothing to
un-publish. Reverting does not delete the newer release; it stops pointing at it.

**Who writes the digest into the templates — ruling of record.** A human reads
the digest off `publish-release` stdout and hand-edits the template. A pod
reading its own digest out of PostgreSQL is EXPLICITLY REJECTED: it reintroduces
the second carrier of release identity that wamn-0h0g.15.102 and `.15.103`
deleted, and it would roll a release without a rollout. `print-release-env`
(wamn-duyl) is the ergonomic improvement on the manual path and has landed —
it removes the transcription step and nothing else, which is why step 3 is a
verb rather than a copy-paste off step 1. It still writes no file; step 4 is
still a person editing two YAML files.

cert-manager is base infrastructure, not a gate prerequisite (wamn-ergz).
`infra/cert-manager.yaml` is the upstream static install **pinned at v1.21.0**
— all three `quay.io/jetstack` images (controller, webhook, cainjector) and
every `app.kubernetes.io/version` label carry that tag. It is vendored verbatim,
as `cnpg-operator.yaml` and `barman-cloud-plugin.yaml` are, so a bump is a
re-fetch of the same URL at the new tag and the diff is purely upstream's:

```bash
curl -sSL -o deploy/infra/cert-manager.yaml \
  https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
# sha256 6e499c3f1ab356abe79a7853911f80cb09c213885bfdf81092fdff142ba63c4a
```

Apply it **before** `barman-cloud-plugin.yaml`, which renders `cert-manager.io/v1`
`Issuer` and `Certificate` CRs and cannot be admitted until the CRDs and the
webhook exist. Gate runbooks apply this file rather than an external
cert-manager release URL — the D6 runbook still names the upstream URL, and
swapping it is tracked on wamn-loha:

```bash
kubectl apply -f deploy/infra/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
```

What it installs: six CRDs, and the controller/webhook/cainjector in namespace
`cert-manager`. What it does **not** do: issue a single certificate, or define
any `Issuer`/`ClusterIssuer` of ours. Issuers belong to their consumers — the
barman plugin ships its own `selfsigned-issuer` in `cnpg-system`, and the two
consumers that make this a bill-of-materials decision rather than a rider on a
TLS change each bring their own:

- `infra/wasmcloud-ca-issuer.yaml` (wamn-0h0g.15.152) — a cluster-scoped
  `ClusterIssuer` over the runtime-operator release's CA, so a host-tier release
  can live in a namespace of its own. It is paired with
  `platform/host-environment-certs.example.yaml`, the per-environment
  `wasmcloud-runtime-tls` / `wasmcloud-data-tls` Certificates.
- `platform/registry.yaml` (wamn-0h0g.15.155 leg B) — a namespaced CA `Issuer`
  plus the registry's serving `Certificate`, so the in-cluster registry serves
  TLS instead of forcing `--allow-insecure-registries` on every reader.

Both borrow the CA the runtime-operator release mints as Secret `wasmcloud-ca`
in `wamn-system`; neither creates a second trust root. The scope difference is
forced by where cert-manager looks for CA key material: a namespaced `Issuer`
resolves `ca.secretName` in its own namespace, which is why the registry's works
with no copy, while a `ClusterIssuer` resolves it in cert-manager's
cluster-resource namespace — `cert-manager`, from the vendored install's
`--cluster-resource-namespace=$(POD_NAMESPACE)`. That one copy of the CA is the
single manual step, documented in `infra/wasmcloud-ca-issuer.yaml`.

One chart, two tiers (wamn-0h0g.15.15, rulings `.13.49` + `.13.50`): the
runtime-operator chart is installed twice with different values, and the tier
split *is* the ruling. `infra/values-wamn.yaml` is the cluster-singleton
operator release — the five CRDs are cluster-scoped and Helm installs them once,
so it is install-once by construction and carries no host groups.
`platform/values-host-<environment>.yaml` is the host tier, one Helm release per
environment, because host images and host groups are per-environment and
per-release values. Install the operator release first; a host release applied
before its CRDs exist has nothing to register into.
