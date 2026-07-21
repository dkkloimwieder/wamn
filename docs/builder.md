# The custom-node builder (5.5 / wamn-0si)

`crates/wamn-builder` turns a tenant's node SOURCE into a signed, SBOM-carrying
OCI artifact the platform will run, inside one isolated, credential-less build
sandbox (the 6.2 threat class: no cluster creds, egress-restricted,
resource/time-limited). It is a one-shot ctl-style verb binary — but its OWN
cargo-ful image (`--target builder-svc`), distinct from the slim cargo-less
`wamn-ctl` image, because it runs the node toolchains (cargo / jco) itself.

## Pipeline (each stage a refuse-on-violation gate)

`wamn-builder build` runs, in order:

1. **Dependency allowlist (5.5c)** — `cargo metadata --offline` resolves the node
   crate's transitive package set, and every crate NAME must be on a pinned
   policy. Enforced BEFORE the build, so an off-policy crate's `build.rs` never
   runs. Refusal: `AllowlistError::DisallowedDependencies` (names the offenders).
2. **Build (5.5b)** — `cargo build --release --target wasm32-wasip2 -p <package>`
   (a Rust cdylib on `wamn-node-sdk` + `wamn-node-guest`, componentized by
   `export_node!`), or `jco componentize` (a single JS/TS ES module). Guest
   artifacts are ALWAYS `--release`.
3. **Import lint (5.5a)** — the built bytes are screened through
   `wamn_host::egress_guard::screen_builder_component`: the tenant PACKAGE
   allowlist (`wasi:sockets` / `wamn:postgres` refused) AND the INTERFACE
   tightening (within `wamn:node` only `payloads`/`credentials`/`control`, within
   `wasi:http` only `outgoing-handler`).
4. **Sign + SBOM (5.5d)** — an ed25519 detached signature over `sha256(wasm)`
   plus a minimal CycloneDX SBOM.
5. **OCI push (5.5e)** — the wasm layer + the `wamn.node.manifest` / signature /
   SBOM annotations, pushed so the wash-runtime host can still pull it.
6. **Deployment emission (5.5f, `--emit-deployment`)** — the serve-node runtime
   manifest with grants DERIVED from the imports.

## 5.5a — interface lint + derived grants

Lives in `crates/wamn-host/src/egress_guard.rs` (reused verbatim by the builder;
the egress_guard's own E13a doc deferred the interface-level lint to 5.5). Two
additions over the package-level classifiers the publish gate already enforces:

- **Interface tightening** — `disallowed_node_interfaces` flags a `wamn:node`
  interface outside `{payloads, credentials, control}` or a `wasi:http` interface
  outside `{outgoing-handler}`. `wasi:sockets` is forbidden outright (off the
  package allowlist). `screen_builder_compiled` runs the package arm first
  (names `wasi:sockets`/`wamn:postgres`) then the interface arm.
- **Derived grants (design-note 7)** — `derive_grants(imports)` →
  `DerivedGrants { host_interfaces, requires_allowed_hosts }`. `allowedHosts` is
  REQUIRED iff `wasi:http/outgoing-handler` is imported and REFUSED otherwise
  (`check_allowed_hosts_grant`) — grants derived from the WIT imports, never
  declared twice. The frozen worlds (`docs/wamn-node.wit`): `world node` imports
  nothing → empty grants; `http-node` imports `wasi:http/outgoing-handler` +
  `credentials` + `control` → those grants + `requires_allowed_hosts`.

## Dependency allowlist policy

`crates/wamn-builder/policy/default-allowlist.toml` — a crate-NAME surface list
(version-agnostic: which crates may appear, not which versions). Pinned to the
ACTUAL transitive closure of `components/samples/sample-node` (48 names:
`serde_json` + the `wamn-node-{sdk,guest}` path + the `wit-bindgen` /
`wit-component` componentization toolchain). `wamn-node-sdk`'s own direct deps are
deliberately `{serde_json}` only. `--allowlist <path>` overrides the default. The
jco path has no cargo graph; its v0 rule is structural (single ES module, no
`package.json` dependency closure).

## Signing / trust model (5.5d)

Greenfield artifact provenance (the existing `wamn-node-invoke` HMAC is
runner→node MESSAGE auth, not provenance; no cosign/sigstore in the tree). We use
`ring`'s `Ed25519KeyPair` (already resolved in the workspace lock via the TLS
stack — no new heavy dep). The signed message is the raw `sha256(wasm)` digest;
verification recomputes it, so a signature binds the exact artifact bytes.

- **Keys**: hex-encoded text. The PRIVATE key is the PKCS#8 document (banked in a
  Secret); the PUBLIC key is the 32-byte raw ed25519 key (the buildproof
  verification fingerprint). `wamn-builder keygen --private-key … --public-key …`.
- **Annotations at push**: `wamn.node.signature` (hex sig),
  `wamn.node.signed-digest` (`sha256:<hex>`), `wamn.node.public-key` (hex pubkey).
- **Secret**: `deploy/platform/builder-signing-key.yaml` documents the shape; the
  main loop generates the real key and creates the Secret.

### SBOM attachment choice

A minimal CycloneDX 1.5 SBOM (`type`/`name`/`version`/`purl pkg:cargo/…` per
component) from `cargo metadata`, attached as an OCI ANNOTATION
(`wamn.node.sbom`) — NOT a layer blob. Justification: a node SBOM (its ~50-crate
closure ≈ a few KB) is small, and an annotation keeps the manifest a SINGLE
`application/wasm` layer (matching the live wash-pushed shape exactly, so
pullability is maximally certain) and lets `buildproof` read it without a second
blob fetch. A large SBOM → an additional layer blob is a deferral.

## OCI media types (the pullable wire shape)

The push is HAND-ROLLED over hyper 1 (`crates/wamn-builder/src/registry.rs`), the
repo's first Rust OCI writer — not `oci-client` — for full control of the
annotations + media types and a wire shape the stub test pins byte-for-byte. The
artifact MUST stay pullable by the wash-runtime host, whose fork pull path
(`crates/wash-runtime/src/oci.rs`, `pull_component` — rev `eef76cd`, lines
422-452) accepts `[WASM_LAYER_MEDIA_TYPE, WASMCLOUD_MEDIA_TYPE]` and takes
`layers.first()`. So:

| element  | media type                                    |
| -------- | --------------------------------------------- |
| manifest | `application/vnd.oci.image.manifest.v1+json`  |
| config   | `application/vnd.wasm.config.v0+json`         |
| layer[0] | `application/wasm` (the raw component bytes)  |

These match oci-wasm 0.5.0 and the LIVE wash-pushed api-gateway artifact
(cross-checked against the in-cluster registry). The wasm is layer[0]; annotations
are ADDITIVE to that single-layer live shape (the live manifest carries none). The
config blob is a minimal WasmConfig-shaped JSON with a fixed `created` (the
artifact is byte-reproducible); the fork pull path does NOT parse it (it takes
`layers.first()`), so the LAYER — not the config — is load-bearing for
pullability. The registry is plain-HTTP `registry:2`
(`registry.wamn-system.svc.cluster.local:5000`); the host pulls with
`--allow-insecure-registries`. TLS push is a deferral.

## Deployment emission — plan-vs-shipped fork (5.5f)

`--emit-deployment <path>` renders the node's runtime manifest. **The plan's
`WorkloadDeployment` form (the operator OCI-fetches the artifact) is UNSHIPPED —
OCI-fetch is wamn-fqg.21's open scope — so v0 emits the SHIPPED `serve-node`
Deployment shape** (`deploy/platform/serve-node.yaml`): `wamn-host serve-node`, a
ConfigMap-mounted node (`--node`), `WAMN_PROJECT` + the credential vault, and the
`--allowed-hosts` arg present iff `wasi:http` is imported (derived, refused
otherwise). The emitted metadata annotations carry the OCI ref + signed digest +
signature so fqg.21's future operator can adopt it. Golden files:
`crates/wamn-builder/tests/golden/{world-node,http-node}.deployment.yaml`.

## buildproof gate

`wamn-gates buildproof` verifies a pushed artifact FROM THE REGISTRY over plain
HTTP (reusing the registry client): the `wamn.node.manifest` annotation parses via
`NodeManifest::from_json` + `is_valid`; `layers[0]` is the pullable
`application/wasm` layer with digest integrity; the signature verifies against
`--public-key` (env `WAMN_BUILDER_PUBLIC_KEY`); the SBOM lists each
`--expect-package`.

## Deferrals (filed as beads)

- **User-source ingestion** — fetching/unpacking untrusted source into the
  sandbox; v0 builds the baked-in `sample-node` fixture.
- **NetworkPolicy enforcement in kind** — `builder-netpol.yaml` is correct but
  INERT under kind's kindnetd (which does not enforce NetworkPolicy); a
  policy-enforcing CNI is needed.
- **Verify-signature-at-deploy / host-pull** — the host does not yet verify
  `wamn.node.signature` before running a pulled node.
- **Registry persistence** — the in-cluster `registry:2` is `emptyDir`
  (EPHEMERAL); artifacts vanish on pod restart (re-push).
- **TS SDK** — the jco path is a raw JS/TS handler; no TypeScript node SDK.
- **npm dependency allowlist** — the jco path only asserts single-module /
  no-`package.json`-dependencies; a real npm SBOM/allowlist is future.
- **TLS OCI push** — the writer is plain-HTTP only.
- **Large SBOM as a layer blob** — SBOMs are annotations today.
