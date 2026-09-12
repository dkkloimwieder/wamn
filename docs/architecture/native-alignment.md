# Native alignment

WAMN uses the public native runtime before adding its own implementation.
Every retained difference needs a concrete benefit and a condition for removal.
A new difference must update this page in the same change.
This page owns those reasons. Other architecture pages own the behavior itself.

## Upstream ownership

The root [Cargo manifest](../../Cargo.toml) pins direct wasmCloud `v2.9.0` source at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
The resolved Wasmtime family is `47.0.4`.
There are no carried upstream patches, and upstream default providers remain disabled.
The existing owner rule forbids restoring the retired fork patches.

Native loading and dispatch own compilation, linking, fresh stores, epoch interruption, and guest cancellation.
WAMN uses public `GuestCall`, `InstancePolicy::Ephemeral`, `ProbeState`, `Liveness`, and bounded flush operations.
Host and executor process construction call native `raise_descriptor_limit()` before sizing descriptor-dependent resources.
Their Tokio runtimes use `event_interval(1)` for timely timer polling.
These choices make no general performance claim.

## Retained WAMN implementations

| Owner | Reason retained | Condition for replacement |
| --- | --- | --- |
| [`wamn:postgres`](data-access.md) | Commands need one exclusive session through admitted SQL and transaction finalization. | `wamn-0ct2.5` retains this owner. Native availability alone does not authorize a second database path. |
| [Import admission](capabilities.md#import-admission) | Publication refuses unsupported authority before distribution. | Preserve the tenant contract when considering any native replacement. |
| [Immutable wiring releases](overview.md#release-identity) | Tenant-owned declarations need admission, rollback identity, and exact provenance. | Reconsider when native links supply equivalent tenant and per-link authorization. |
| [Fresh invocation stores](execution.md#native-dispatch) | Compiled-byte reuse must not retain guest request state or share admitted authority. | Reuse requires tenant isolation, request-state separation, `maxConcurrency = 1`, and ephemeral fallback on saturation. |
| [Guest normalization](components.md#guest-build-identity) | Standard-library imports must satisfy the closed tenant capability set. | A replacement build must preserve that actual import boundary. |
| `tools/build-components` | Per-guest builds preserve digest identity and admission integration. | Native build and OCI publication must supply the same declared selection and admission requirements. |
| Operator Event permissions | Chart `2.9.0` lacks the modern `events.k8s.io` permission in watched namespaces. | Remove the scoped `create,patch` Role only when the pinned chart grants it. |
| [Object storage](capabilities.md#object-storage) | Binding ownership requires bucket and prefix confinement plus complete-body writes. | Adopt stable `wasi:blobstore` when upstream supplies the required binding behavior by default. |
| Platform extensions | Extension installation changes the whole database and needs administrator authority. | A package-scoped mechanism must prevent effects on neighboring packages. |
| [HTTP transport](capabilities.md#outbound-http) | Native connector access does not preserve pinned peers and the required client identity and limits. | Public native APIs must preserve peer pinning, credential generations, aggregate bounds, and exact outcomes. |
| OCI certificate loading | WAMN artifact layouts need their own clients, scoped insecure-registry policy, and transport timeouts. | A public native certificate accessor or client constructor must preserve those artifact contracts. |
| [Registry credentials](capabilities.md#registry-credentials) | Server credentials require an explicit path, exact registry authority, and a narrow credential shape. | Native code must accept that path without global environment changes or broader credential forms. |
| [Expected-host routing](execution.md#routing-and-availability) | A released hostname without a binding needs a temporary-unavailability response. | Native routing must preserve the same distinction without rewriting application responses. |

Any reuse change must include its resource rules and runtime implementation together.
The adoption decision requires no benchmark.

WASI-Virt is pinned to `448f6df8f688cee5d6995e96b1ffc31f9bf00742` with adapter SHA-256 `28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632`.
The capability rows follow the adapter's emitted versions, not the authored WIT versions.
A pin change must preserve that relationship.
Build shape serves admission policy and cannot weaken it.

The OCI readers preserve WAMN configuration blobs and media types.
They require registry-scoped `HttpsExcept`, connect and read timeouts, and `ManifestUnknown` discrimination for exact republishing.
A generic upstream component transfer cannot replace them by dropping those facts.
`wamn-kdhw` owns the remaining certificate and explicit-credential integration conditions.

## Current limits

The [routing contract](execution.md#routing-and-availability) retains the native 404 and readiness limitations.

The native operator can exit after an initial scheduler NATS dial timeout.
`wamn-10yt.76` retains the retry-gap work and the unresolved fault-time liveness delay.
Accepted supervised restarts do not establish the cause or removal of that delay.
A successful recovery without a startup refusal cannot establish recovery after that refusal.

The [lifecycle contract](execution.md#process-lifecycle) retains the unbuilt automation producer and dependent active-work shutdown case.

Tenant raw-socket admission remains refused.
`wamn-d0w4` must be resolved before admitting a raw-socket guest.
The [HTTP capability](capabilities.md#outbound-http) has a separate policy owner.
A change to its external address-enforcement boundary requires its own security decision.

Former 2.8 patches and detailed run observations remain in the [prior source record](https://github.com/dkkloimwieder/wamn/blob/8f38861387debc392b4b57c92f4cc052974f4f02/docs/architecture/native-alignment-ledger.md).
The old private P2 phase spans and missing-handle 503 patch are not present in current upstream code.
Historical measurements do not establish current diagnostic coverage or close unresolved behavior.
