# Native alignment

WAMN uses the public native runtime before adding its own implementation.
Every retained difference needs a concrete benefit and a condition for removal.
A new difference must update this page in the same change.
This page owns those reasons. Other architecture pages own the behavior itself.

## Upstream ownership

The root [Cargo manifest](../../Cargo.toml) pins direct wasmCloud `v2.12.1` source at `8b0e24cb6610642783e3f16f5f1eac45ff2cb5aa`.
The resolved Wasmtime family is `48.0.8`.
`wash-runtime` resolves directly to that upstream revision without a workspace patch.
Upstream default providers remain disabled.
Application components use the upstream linker. Undeclared WASI imports no longer fail at instantiation through a WAMN-scoped linker; application admission still refuses `wasi:sockets`.

The [2.12.0](https://github.com/wasmCloud/wasmCloud/releases/tag/v2.12.0) and [2.12.1](https://github.com/wasmCloud/wasmCloud/releases/tag/v2.12.1) runtime changes include:

- Linkers seal after binding and retain their prepared links. Linear-memory slots and core-instance counters have separate budgets.
- Derived cgroup memory budgets cap at RAM plus swap. Process-footprint checks refresh on each start beyond the high-water mark.
- Plugin configuration keeps secrets typed until a consumer reads them. Plugin-served WASI interfaces use plugin precedence instead of builtin overrides.
- Linked async tasks release on stop, service failures propagate, and stalled NATS secret reads allow shutdown.
- HTTP span names align, P3 responses stream directly, and guests can still write an unsent outgoing HTTP body.
- New TLS connections refresh the client identity. Native socket handling fixes UDP progress, reconnects, and shared receive budgets.
- Wasmtime 48.0.8 fixes UDP permission checks, cross-engine GC type confusion, typed-reference panics, borrowed-resource accounting, and P2 HTTP content lengths.

Upstream NATS, PostgreSQL, telemetry, and S3 plugins also gain input and transport fixes.
Those changes apply where their native implementations run. WAMN-owned clients and plugins retain their own behavior.
The chart and operator pins remain separate from this runtime source update.

Native loading and dispatch own compilation, linking, fresh and warm stores, epoch interruption, and guest cancellation.
WAMN uses public `GuestCall`, `InstancePolicy::Ephemeral`, `InstancePolicy::Warm`, `ProbeState`, `Liveness`, and bounded flush operations.
Host process construction calls native `raise_descriptor_limit()` before sizing descriptor-dependent resources.
Its Tokio runtime uses `event_interval(1)` for timely timer polling.
These choices make no general performance claim.

## Retained WAMN implementations

| Owner | Reason retained | Condition for replacement |
| --- | --- | --- |
| [`wamn:postgres`](data-access.md) | Commands need one exclusive session through admitted SQL and transaction finalization. | Native availability alone does not authorize a second database path. |
| [Import admission](capabilities.md#import-admission) | Publication refuses unsupported authority before distribution. | Preserve the tenant contract when considering any native replacement. |
| [Immutable wiring releases](overview.md#release-identity) | Tenant-owned declarations need admission, rollback identity, and exact provenance. | Reconsider when native links supply equivalent tenant and per-link authorization. |
| [Invocation authority](execution.md#native-dispatch) | Deployment-trusted code can reuse native instances. Host authority remains per invocation. Guest data and task cleanup depend on reviewed component correctness. | Native dispatch owns store boundaries, overflow, and retirement. Unreviewed units remain fresh. |
| [Guest normalization](components.md#guest-build-identity) | Standard-library imports must satisfy the closed tenant capability set. | A replacement build must preserve that actual import boundary. |
| [`wamn build`](../operations/building.md#native-programs-and-app-components) | Per-guest builds preserve digest identity and admission integration. | Native build and OCI publication must supply the same declared selection and admission requirements. |
| Operator Event permissions | Chart `2.11.0` lacks the modern `events.k8s.io` permission in watched namespaces. | Remove the scoped `create,patch` Role only when the pinned chart grants it. |
| [Count mode](execution.md#native-dispatch) | Chart `2.11.0` defaults guest memory and raw-socket egress to enforce. The host keeps guest memory in count and defaults raw-socket egress to enforce. Guest memory count mode records growth that enforce would refuse and allows it. | Guest memory remains in count pending an owner ruling. Socket enforce mode blocks refused traffic; the existing socket flag still accepts count. |
| [Object storage](capabilities.md#object-storage) | Binding ownership requires bucket and prefix confinement plus complete-body writes. | Adopt stable `wasi:blobstore` when upstream supplies the required binding behavior by default. |
| Platform extensions | Extension installation changes the whole database and needs administrator authority. | A package-scoped mechanism must prevent effects on neighboring packages. |
| [HTTP transport](capabilities.md#outbound-http) | Native connector access does not preserve pinned peers and the required client identity and limits. | Public native APIs must preserve peer pinning, credential generations, aggregate bounds, and exact outcomes. |
| OCI certificate loading | WAMN artifact layouts need their own clients, scoped insecure-registry policy, and transport timeouts. | A public native certificate accessor or client constructor must preserve those artifact contracts. |
| [Registry credentials](capabilities.md#registry-credentials) | Server credentials require an explicit path, exact registry authority, and a narrow credential shape. | Native code must accept that path without global environment changes or broader credential forms. |
| [Expected-host routing](execution.md#routing-and-availability) | A released hostname without a binding needs a temporary-unavailability response. | Native routing must preserve the same distinction without rewriting application responses. |

Any reuse change must include its resource rules and runtime implementation together.
The adoption decision requires no benchmark.

WASI-Virt is pinned to `448f6df8f688cee5d6995e96b1ffc31f9bf00742` with adapter SHA-256 `28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632`.
The `wasi:*` capability rows name the family of the adapter's emitted versions, not the authored WIT versions.
A pin change must keep the emitted versions in that family and at or below the ceiling.
Build shape serves admission policy and cannot weaken it.

The OCI readers preserve WAMN configuration blobs and media types.
They require registry-scoped `HttpsExcept`, connect and read timeouts, and `ManifestUnknown` discrimination for exact republishing.
A generic upstream component transfer cannot replace them by dropping those facts.

## Current limits

The [routing contract](execution.md#routing-and-availability) retains the native 404 and readiness limitations.

The native operator can exit after an initial dial timeout to the one platform NATS.
Through a NATS outage, Kubernetes restarts the operator with a backoff of at most 5 minutes, and the operator recovers when NATS returns.
A fault-time liveness delay remains. Supervised restarts do not remove it.

The [lifecycle contract](execution.md#process-lifecycle) covers the combined host shutdown path and its connection retirement limit.

Tenant raw-socket admission is refused.
The [HTTP capability](capabilities.md#outbound-http) has a separate policy owner.
A change to its external address-enforcement boundary requires its own security decision.

## PostgreSQL replication dependency

The root manifest uses the released `pg_walstream` 0.9.0 crate from `vendor/pg-walstream`.
The local patch returns typed keepalive events so idle readers can report acknowledged progress.
The raw event API retains its upstream behavior.
Upstream 0.9.0 includes the FAILOVER slot syntax fix.
See [the patch record](../../vendor/pg-walstream/WAMN-PATCH.md).
