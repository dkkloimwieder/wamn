---
status: item-local design
plan-item: 2B
bead: wamn-ko5r.5
date: 2026-08-01
---

# Connections: portable requirements, environment bindings

This document fixes the minimum design for [PLAN item 2B](../PLAN/PLAN.md#2b--connections--the-env-boundary-for-anything-external). It is subordinate to
that plan and builds on the canonical resolved-node contract introduced by
`wamn-4u7p.8` (`95eb37a`). It does not select an authoring surface or replace
platform host policy or cluster network policy.

The invariant is:

> An artifact declares a typed, portable connection requirement. An
> environment owns connection instances. An environment-specific release binds
> each requirement to an instance. No endpoint, proxy, TLS setting, credential
> material, or environment identifier enters artifact or execution-bundle
> identity.

The first supported external type is HTTP. Postgres is explicitly excluded from
the provider prototype and remains on its existing trusted execution path.

## Identity and ownership

The canonical [`ResolvedNodeContract`](../../crates/node/manifest/src/lib.rs)
already records `connection-requirements` as sorted `(requirement-type,
contract)` pairs. Those pairs state what an executable can consume and are part
of artifact and execution-bundle identity. They do not identify an environment
instance.

A flow artifact adds logical requirements such as `erp`. A graph node refers to
that logical name; it never carries an absolute endpoint. Multiple nodes may use
one requirement, and two requirements of the same type may bind to different
instances.

| Record | Stable key | Required fields | Owner and mutability |
|---|---|---|---|
| Connection requirement | `(artifact_hash, requirement_name)` | type, exact connection contract, required recovery class, portable constraints | Artifact; immutable. Changing it creates a new artifact version. |
| Connection instance | `(tenant, environment, instance_id)` | type, lifecycle status, pointer to active generation | Environment; stable identity. It is not a release member. |
| Instance generation | `(instance_id, generation)` plus definition hash | canonical authority, TLS and redirect policy, proxy reference, credential-set handle, semantic attestations | Environment; immutable after creation. Activation is controlled. Secret material is never stored here. |
| Connection binding | `(release_id, artifact_hash, requirement_name)` | instance id, validation result, binding status | Environment-specific release; immutable with that release. It points to the instance, not a mutable copy of its fields. |
| Effect connection record | occurrence and attempt identity | requirement name, instance id, generation and definition hash, credential generation, effective recovery class | Durable run history; append-only. No secret material. |

`requirement_name` is an artifact-local identifier, not an environment name.
The connection `type` and `contract` must agree with one of the consuming node's
canonical `connection-requirements`. A requirement may narrow a type contract
(for example, require idempotency-key support); it may not add a capability that
the resolved node contract lacks.

The following changes have deliberately different consequences:

| Change | Consequence |
|---|---|
| Requirement name, type, contract, or portable recovery constraint | New artifact identity. |
| Node connection import or WIT contract version | New resolved-node contract, artifact identity, and execution-bundle identity. |
| Host adapter executable/revision | New adapter entry in `ExecutionBundleIdentity`; recompose the affected bundle. |
| Requirement-to-instance binding | New environment release; artifact bytes remain identical. |
| Endpoint, TLS, redirect, proxy, credential-set reference, or attestation | New immutable instance generation; no artifact or bundle change. |
| Credential secret rotation without a connection-definition change | New credential generation recorded by each attempt; no secret or credential generation enters an artifact. |

For the initial `0.1.0` contract, consumers require the exact WIT package
version. An additive implementation that preserves that package needs no
artifact change. Any material interface change uses a new package version and
therefore invalidates artifact and bundle identities mechanically; compatibility
is never inferred from an unchanged string.

### What the type contract asserts

The connection type contract defines portable semantics, not environmental
truth. It fixes:

- protocol operations and ABI identity;
- which request fields are author-controlled and which authority, credential,
  TLS, redirect, and proxy fields remain environment-controlled;
- host-adapter obligations, including credential and idempotency-key injection;
- the conservative recovery default; and
- the vocabulary, parameters, and exact semantics of stronger recovery claims.

The boundary is: a resolved node says which type contract and recovery
mechanisms its executable supports; an artifact requirement selects a claim and
minimum parameters from that contract; an authorized environment connection
administrator attests that one immutable instance generation satisfies them;
binding validation matches the two; and each effect attempt records the exact
claim and generation admitted. Neither a node publisher nor a flow author can
attest facts about a target environment.

HTTP `0.1` defaults to `never-replay` and defines one strengthening claim,
`stable-key-dedup-v1`:

1. The adapter, not node configuration, transmits the engine-generated stable
   key through the contract-owned `Idempotency-Key` mechanism.
2. The attested receiver deduplicates concurrent and later requests within one
   named idempotency domain for at least the requirement's minimum retention.
3. Repeating the same key with the same canonical operation fingerprint cannot
   repeat the externally visible effect. It returns the same terminal outcome,
   or a contract-defined duplicate result the adapter normalizes to that
   outcome.
4. Reusing the key with a different fingerprint is rejected. The
   contract-owned fingerprint covers canonical method, connection-relative
   target, semantic headers, and body digest.
5. Every authority and failover target admitted by the generation shares that
   idempotency domain.

Header propagation alone, HTTP method idempotence, and an unscoped assertion of
"safe" do not satisfy the claim. HTTP `0.1` defines no `replay` strengthening:
GET/HEAD names prove neither stable responses nor absence of receiver-specific
effects. A later contract version may define a stronger read claim with precise
semantics and evidence.

The portable requirement therefore holds the claim name and minimum dedup
retention. The instance-generation attestation holds the idempotency-domain
identifier, achieved retention, approving principal, and evidence reference.
Decision bead `wamn-ko5r.4` separately owns evidence freshness and invalidation;
this decision defines what that evidence must establish.

## Lifecycle and compatibility

Requirements are checked when a release is assembled or promoted, before it can
accept work:

1. The binding exists in the target environment.
2. Its instance type and exact contract satisfy the portable requirement.
3. The active generation is mechanically valid under connection, platform-host,
   and cluster-network policy.
4. Every semantic attestation needed by the required recovery class is present,
   unrevoked, and within its validity window.
5. The referenced credential set exists and has the credential kind the
   connection contract requires.

Failure rejects publication or promotion with the requirement name and failed
condition. It does not defer failure to first dispatch and does not select a
weaker recovery class.

A proposed generation is staged, validated against **every active binding** of
the instance, then activated with a compare-and-swap against the previously
active generation. Validation and activation are one serialized operation. If
any binding would become unsatisfied, activation fails and the previous
generation remains active. Disabling a binding or creating a differently scoped
instance is a separate explicit operator action.

Mechanical compatibility covers type and contract identity, required fields,
canonical authority shape, TLS/redirect/proxy policy, credential kind, and the
two outer egress ceilings. Semantic claims are attributable attestations. For
HTTP `stable-key-dedup-v1`, binding validation compares the requirement's
minimum retention with the generation's attested retention and requires the
attested idempotency domain to cover every admitted authority. Decision bead
`wamn-ko5r.4` owns the durable evidence, freshness, and invalidation policy.
Expiry or invalidation must make a binding requiring `idempotent-with-key`
unsatisfied and must never change it to `never-replay`.

Non-secret generations and their evidence are retained while an active attempt
or retained audit/replay seed refers to them. Credential material follows its
own revocation and retention policy. Losing access to a recorded credential
generation causes an explicit recovery refusal rather than substitution.

## One canonical HTTP authority resolver

All standard and custom HTTP nodes call the typed connection adapter below.
They do not import generic `wasi:http/outgoing-handler`, accept absolute URLs,
or read proxy environment variables. The adapter is the sole constructor of an
outbound request and applies this algorithm:

1. Load the trusted release binding and its current active, compatible
   generation. The caller supplies only the artifact-local requirement name and
   request-local method, relative path/query, headers, and body.
2. Canonicalize the generation's authority to `(scheme, ASCII host, effective
   port)`. Reject user-info, fragments, ambiguous encodings, and unsupported
   schemes. Resolve a relative path against the configured base path without
   permitting `..` or encoded separators to escape it.
3. Form the logical destination from that authority. Request fields cannot
   replace scheme, host, port, TLS identity, proxy, or credentials.
4. Apply connection authority, platform host policy, and cluster network policy
   as an intersection. A denial at any layer is final.
5. Resolve DNS through the host-controlled resolver. Connect only to an address
   returned by that resolution that passes the outer network ceiling, bind the
   selected address to this request, and retain the canonical DNS name for HTTP
   `Host` and TLS identity. A literal address is allowed only when the generation
   itself names that literal and both outer ceilings allow it.
6. Ignore guest and process proxy settings. If the generation declares a proxy,
   the host connects only to that proxy under the outer ceilings and fixes the
   CONNECT/origin target to the already-authorized logical destination.
7. Do not follow redirects automatically. A redirect may be followed only after
   re-entering this resolver; the minimum policy permits the same canonical
   authority and base-path scope only. Cross-authority redirects are denied.

DNS therefore chooses a transport address but never destination authority;
redirects initiate a fresh decision but cannot add an authority; and a proxy is
a configured transport hop, not an alternate destination. Resolution and the
actual connect use the same selected address, closing the check/use DNS-rebinding
gap.

The adapter records the canonical authority decision and generation identity,
not resolved secret values. Existing host and cluster policies remain the outer
ceilings, so a connection can narrow but never widen them.

## Minimum typed connection ABI

The first contract is `wamn:connection/http@0.1.0`. Its minimum WIT shape is:

```wit
package wamn:connection@0.1.0;

interface http {
  record header { name: string, value: list<u8> }
  record request {
    requirement: string,
    method: string,
    path-and-query: string,
    headers: list<header>,
    body: option<list<u8>>,
    idempotency-key: option<string>,
  }
  record response {
    status: u16,
    headers: list<header>,
    body: list<u8>,
  }
  variant connection-error {
    unbound,
    incompatible,
    authority-denied,
    attestation-invalid,
    credential-unavailable,
    timeout,
    transport(string),
  }
  send: func(request: request) -> result<response, connection-error>;
}
```

This is intentionally smaller than a general HTTP client. It makes the logical
requirement explicit, accepts no absolute URI or proxy, and carries the stable
idempotency key as a distinct field so the adapter—not arbitrary node code—owns
its required header propagation. Streaming and protocol-specific extensions
require a versioned contract change; they are not hidden in JSON.

The host adapter receives trusted execution context containing release,
artifact, node, occurrence, and attempt identity. It verifies that the named
requirement belongs to the artifact, that the executing node may consume its
type contract, and that the release binds it. It installs credentials only after
that check, invokes the canonical resolver, and writes the effect connection
record before network transmission. A custom component gets this interface by
declaring the import; it never receives a generic raw-egress grant.

This ABI is also the dependency carried by 2A's first capability-bearing proof.
That proof composes a node importing `wamn:connection/http@0.1.0`, verifies the
composed artifact imports that typed capability and no generic HTTP/socket
capability, and includes the exact adapter revision in execution-bundle
identity. Dev and prod then run the same artifact and bundle against distinct
bindings.

## Attempt identity and recovery

A connection generation is pinned per external effect attempt, before send. A
retry or recovery of that attempt may use only the recorded instance generation,
definition hash, credential generation, and effective recovery class. The
durable decision in `wamn-ko5r.1` does **not** retarget an uncertain attempt to a
newer generation, even when an operator asserts a shared idempotency domain.
The retry uses the recorded claim, attestation, operation fingerprint, and stable
key and proceeds only when its recovery class permits redispatch and the exact
pinned definition, credential generation, and authority remain usable. Otherwise
it refuses explicitly. Shared-domain evidence may admit a new generation for a
later occurrence; it is not substitution authority for the existing attempt.
Version 1 defines no retarget protocol.

For `idempotent-with-key`, the record is durable before send and includes the
stable occurrence idempotency key, canonical operation fingerprint, and exact
`stable-key-dedup-v1` attestation that admitted the class. For `never-replay`, a
lost outcome after send becomes `effect-uncertain`.
If the old endpoint or credential generation cannot be reacquired, recovery
refuses explicitly. A later, distinct node occurrence resolves and records the
then-active compatible generation independently.

The replay seed therefore answers both questions without making environment data
part of artifact identity: which executable semantics ran, from the pinned
resolved-node contract; and which environmental authority justified the effect,
from the effect connection record.

## Host-component provider prototype

After the in-process adapter establishes the contract, prototype one
feature-gated wasmCloud v2.6 host-component provider for HTTP against a local
deterministic echo/idempotency fixture. Do not use Postgres or production
credentials.

At `on_workload_item_bind`, the provider receives exact caller identity,
validates the resolved binding and immutable generation, rejects incompatible
configuration, and establishes its bounded client pool before the first call.
It exposes only `wamn:connection/http@0.1.0`. Each call still uses the canonical
authority resolver and durable attempt identity supplied by the trusted host;
the provider does not become a second binding authority. At
`on_workload_unbind` it drains and destroys the pool and removes all
caller-indexed grants and generation state. Rebinding the same workload id
starts empty.

The experiment decides only whether bind-time validation, pool lifecycle, caller
identity, and typed capability delivery are usable at acceptable cost. It does
not enable host-component plugins globally or make the provider the production
default.

## Named proof shape

| Proof | Positive witness | Required negative or mutation witness |
|---|---|---|
| `connection-contract-proof` | Canonical manifest, artifact, and bundle round-trip the exact HTTP contract and adapter identity; a conforming `stable-key-dedup-v1` fixture collapses concurrent and later duplicates to one effect and one terminal outcome. | Removing/changing type, contract, claim parameters, or adapter identity changes or invalidates the appropriate identity; environment fields in canonical bytes, key reuse with a different operation fingerprint, header-only false attestations, and method-only `replay` all fail. |
| `connection-publish-proof` | The same artifact publishes in dev and prod with compatible, different bindings. | Missing/wrong-type binding, stale attestation, or recovery mismatch rejects before activation. |
| `connection-authority-proof` | Relative requests reach each environment's fixture through all three policy layers. | Absolute authority injection, base-path escape, cross-authority redirect, DNS rebinding, literal-IP substitution, undeclared proxy, and either outer-policy denial all fail. Bypassing the canonical resolver makes the gate fail. |
| `connection-generation-proof` | A compatible staged generation activates atomically and a later occurrence uses it. | An incompatible or concurrently stale activation is refused and the previous generation stays active. |
| `connection-recovery-proof` | Crash-after-send recovery uses the recorded generation, credential generation, key, and effective class; a later occurrence may use the new active generation. | Retargeting the uncertain attempt, substituting credentials, or silently downgrading recovery fails; unavailable old material produces explicit refusal. |
| `connection-provider-proof` | The local HTTP provider validates and prewarms at bind, serves only the exact caller, and tears down at unbind. | Calls before bind, from another caller, or after unbind fail; rebind inherits no grants or pool state; the feature remains absent from normal deployed inventory. |
| `connection-2a-integration-proof` | A capability-bearing node composes against the typed HTTP import and the same bundle runs under dev/prod bindings. | Artifact inspection finds no generic HTTP/socket import; changing only environment binding does not change artifact or bundle hash. |

These proofs are gates, not documentation assertions. Each security-sensitive
negative must be exercised by a deliberate mutation that would create a false
green if the named enforcement point were removed.
