# Seam template: the procedure a new platform capability follows

Status: DRAFT 2026-09-06, for owner review. Written under `wamn-7tva.1` as the
first deliverable inside the production-counts charter.

`docs/poc/poc-application-portfolio.md:55-56` states the rule that every new
seam follows a seam template. It describes that template as WIT plus host
plugin plus admission facts. No template document existed. This is that
document.

**Every claim below carries a `file:line` or a bead id. A claim with neither is
a design choice, and it says so in the sentence.**

## 0. Words this document uses

- **Seam.** One platform capability that a tenant component can reach. An
  interface carries it. Code inside the platform implements it. The platform's
  own admission tables record it.
- **WIT.** WebAssembly Interface Types. It is the interface description
  language for a component's imports and exports.
- **Guest.** A WebAssembly component that a tenant authors and pushes.
- **Host plugin.** Rust code inside the platform. It implements an imported
  interface on the guest's behalf. The guest calls the interface. The plugin
  answers.
- **Posture.** The declared security class of a capability package. It is
  `Ambient` or `Effect` (`crates/platform/component-policy/src/lib.rs:72-77`).
- **Admission.** The step that decides whether pushed component bytes become a
  workload.
- **Binding.** The per-environment act of pointing a declared requirement at a
  real instance.

## 1. What this template is derived from

It is derived from the blobstore seam **as it landed in the tree**. It is not
derived from the portfolio line that named it.

The design is `docs/poc/wms-prep-spec.md` section 2b (`:114-153`). The ledger
row is `docs/architecture/native-alignment-ledger.md:21`. The work is beads
`wamn-jrpw`, `wamn-jpxo`, `wamn-jpxo.1`, `wamn-362o.41` and `wamn-362o.26`.

The portfolio line names three artifacts. The landed seam took **twelve ordered
steps**. They cross five crates, two services, one deploy tier, one conformance
suite and one guest component. The undercount matters. The omitted steps are
the ones that cost the blobstore seam a live refusal. That refusal came after
admission had already passed (`wamn-362o.41`,
`crates/platform/runtime/src/component_admission.rs:259-260`).

Blobstore is execution one, and it is still the only one. MQTT ingress for
application 2 WAS execution two (`docs/poc/poc-application-portfolio.md:13`).
The owner withdrew that on 2026-09-07: NATS captures MQTT natively, so app 2
runs no step of this template (`wamn-7tva.1`).

**This template therefore rests on one execution.** The named trigger for a
second one is a guest that CALLS a new capability. MQTT publish FROM a
component is the candidate, and `docs/exe-model.md:86` already lists it among
the next node-ABI consumers after blob-put. A guest that RECEIVES something
does not need a seam, because the platform delivers to it. The second
execution is what proves the shape. Correct a wrong step there. Do not work
around it.

### The blobstore seam, in the order it landed

| # | Commit | What it added |
|---|---|---|
| 1 | `1d51e647` (`wamn-jrpw`) | the capability registry itself, replacing the admission heuristics |
| 2 | `09874d6c` (`wamn-jpxo`) | the vendored WIT, the host bindgen world, and the connection vocabulary |
| 3 | `508f4fcd` (`wamn-jpxo`) | confinement walls and the write-intake ceiling |
| 4 | `61086dd5` (`wamn-jpxo`) | host bindings generated from the world |
| 5 | `9b975ed0` (`wamn-jpxo`) | store failures mapped onto the contract's own error variant |
| 6 | `f911d78a` (`wamn-jpxo`) | the bounded stream drain and the confined object-store layer |
| 7 | `aa8b592d` (`wamn-jpxo`) | the sixteen method bodies |
| 8 | `caa88d9b` (`wamn-jpxo`) | an authorized snapshot resolved into a confined binding |
| 9 | `10001385` (`wamn-jpxo`) | the plugin registered, and live binding resolution |
| 10 | `f18a2045` (`wamn-jpxo`) | a per-effect span and a latency series |
| 11 | `b28a9a7f` (`wamn-jpxo`) | ledger row 9 |
| 12 | `168f15cd` (`wamn-jpxo`) | `blob-put`, the palette node that imports the capability |
| 13 | `3bcbd39d` (`wamn-jpxo.1`) | the capability working in released deployments |
| 14 | `819790ee` (`wamn-362o.26`) | a disposable object store inside the journey, and its binding |

`wamn-362o.41` is not in that list. It was a defect found later. Step 8 of this
template exists so that the next seam does not repeat it.

## 2. The procedure

Do the steps in this order. Each step names its artifact. Each step also names
what refuses after a skip.

### Step 1: Decide whether to adopt a contract or author one

Do this first. When a stable published contract already carries the verbs you
need, adopt it. When it does not, author a new `wamn:*` package.

Blobstore adopted `wasmcloud:blobstore@0.1.0` exactly and singly
(`docs/poc/wms-prep-spec.md:116`).

JetStream authored a new package instead. The contract records the reason. The
pinned fork's only messaging WIT has no ack, no nack, no term, no durable
consumers, no redelivery count and no headers. A guest therefore cannot
participate in stream deduplication
(`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:5-16`). That file
also states the rule that a new namespace beats a forked one.

Record the choice and its reason. The ledger row in step 3 is where it lives.

**If you skip this step:** you find the missing verbs after the method bodies
are written. That is what happened before `wamn:jetstream` existed.

### Step 2: Pin and vendor the contract, then declare a host world

Copy the exact contract file into the runtime crate's WIT dependencies. Add one
host bindgen world. That world imports exactly the interfaces the plugin
implements.

- Artifact: `crates/platform/runtime/wit/deps/<vendor>-<package>/package.wit`.
- Artifact: one `world` block in `crates/platform/runtime/wit/world.wit`. The
  blobstore world is `crates/platform/runtime/wit/world.wit:58-67`. It imports
  three interfaces and nothing else.

Pin one version. Admission matches the version exactly. There is no
normalization and there are no ranges
(`crates/platform/component-policy/src/lib.rs:101-103`).

**If you skip this step:** there is no world to generate bindings from. The
plugin crate has nothing to compile against.

### Step 3: Write the ledger row

Add one row to `docs/architecture/native-alignment-ledger.md`. Row 9 is
blobstore's (`docs/architecture/native-alignment-ledger.md:21`).

The row names three things.

1. What the deviation buys. Blobstore's answer is confinement that the contract
   does not carry.
2. Which verbs never succeed, and why. Blobstore splits them into two
   categories with different remedies. Policy refuses `copy-object` and
   `move-object`. The backend cannot satisfy `info`.
3. The re-convergence trigger. Blobstore's trigger is the `wasi:blobstore`
   draft stabilizing plus upstream binding it by default.

Standing trigger 7 requires the row in the same change as the deviation
(`docs/architecture/native-alignment-ledger.md:93`). In the blobstore run the
row landed at `b28a9a7f`. That is the same bead and the same day as the plugin
commits. It is not the same commit. **The bead satisfied the rule in practice.
The rule says the commit.** A reviewer states that gap out loud. Quietly
widening the rule is the failure mode.

**If you skip this step:** standing trigger 7 refuses the change.

### Step 4: Add the capability registry row

Add one `CapabilityRow { package, version, posture }` to `CAPABILITY_REGISTRY`
in `crates/platform/component-policy/src/lib.rs:130`. Blobstore's row is at
`:147-151`.

The registry is closed and it fails closed. Absence refuses an import. A
namespace opinion does not
(`crates/platform/component-policy/src/lib.rs:99-100`). Rows record what the
host offers. Rows do not record what a guest currently imports (`:114-117`).

Then update the conformance suite.
`tests/conformance/tests/capability_registry.rs:136-151` pins the registry size.
It also pins the exact list of effect-posture packages. Both assertions go red
until the new row is written into them deliberately. That friction is the
point.

**If you skip this step:** no guest can import the package. The seam is
unreachable from a tenant component, even while the plugin runs.

### Step 5: Add the connection vocabulary

Do this step only for a capability that needs per-environment coordinates.
Blobstore needed them. It has four artifacts.

1. A variant on `ComponentConnectionType`, plus the exact package its import
   maps to (`crates/catalog/model/src/component_library.rs:85` and `:99`). The
   `CONNECTION_TYPES` array at `:91` does not compile until the new variant is
   listed there.
2. A descriptor constructor minted by the platform, never authored
   (`crates/catalog/model/src/connection.rs:123-131`). It names the requirement
   type and the exact contract string. The platform owns connection semantics
   and selects them whole. An author never writes them field by field
   (`crates/catalog/model/src/component_library.rs:104-109`).
3. The push-time translation from a declared alias to the portable record
   (`services/ctl/src/push_component.rs:558-571`).
4. The bind-time coordinate list. It is exactly the set the plugin reads at
   resolve time (`services/ctl/src/bind_connection.rs:55`, `:61`, `:70`).
   Blobstore's list is `endpoint`, `container`, `prefix`.

Decide the ownership split here. Write it into the descriptor. For blobstore
the environment owns endpoint, container and prefix. The author owns only an
object key relative to the prefix
(`docs/architecture/native-alignment-ledger.md:21`).

**If you skip this step:** a component can declare no alias for the capability,
so nothing binds. A definition that misses a coordinate the plugin reads is
also refused at bind time (`services/ctl/src/bind_connection.rs:17-22`).

### Step 6: Build the confinement before the method bodies

The blobstore run got this ordering right. This template most wants to preserve
it. `508f4fcd` landed the walls before any method existed.

- Artifact: a module that turns the descriptor's ownership split into refusals.
  For blobstore that is
  `crates/platform/runtime/src/plugins/wamn_blobstore/confinement.rs:1-10`. An
  author can name an object. An author can never name a container.
- Artifact: a module that bounds any body the guest streams in. It refuses to
  commit a body it cannot prove complete. For blobstore that is `intake.rs`,
  described at
  `crates/platform/runtime/src/plugins/wamn_blobstore/mod.rs:10-17`.

Two decisions from that run are worth copying as questions, not as answers.
Refuse a dangerous token as a path segment, never as a substring. A substring
rule refuses legitimate names. Never double a separator. Two spellings of one
logical name break any deterministic-key rule built on top
(`crates/platform/runtime/src/plugins/wamn_blobstore/mod.rs:19-29`).

**If you skip this step:** the ownership split stays a declaration. It never
becomes a refusal
(`crates/platform/runtime/src/plugins/wamn_blobstore/confinement.rs:3-8`).

### Step 7: Bindings, then the error map, then the method bodies

Do these three in order. Blobstore did (`61086dd5`, then `9b975ed0`, then
`aa8b592d`).

- Generate host bindings from the world of step 2. Artifact: a `bindings`
  module.
- Map every backend failure onto the contract's own error variant. Artifact: a
  `wit_error` module. Do this before the bodies. Then no body invents a second
  error taxonomy.
- Write the bodies. Artifact: a `host` module. Any verb you cannot satisfy
  becomes a line in the ledger row of step 3, in one of that row's two
  categories.

**If you skip the error map:** each body invents its own translation. That is
the parallel wire taxonomy the repository's Rust convention forbids
(`CLAUDE.md`, the Rust section).

### Step 8: Fix every classifier that keys on a namespace

The portfolio line omits this step. It is the step that cost the blobstore seam
a live failure.

Two copies of one classifier decide a single question. Is an import a platform
capability, or a cross-package application call? At the time blobstore landed,
one copy had moved onto the registry. The other kept a namespace rule. That
rule read "not `wasi` and not `wamn` means an application call". The first push
of a blobstore guest then refused `wasmcloud:blobstore/*` as an undeclared
operation dependency. The registry had already admitted it
(`crates/platform/runtime/src/component_admission.rs:259-260`,
`crates/catalog/model/src/component_library.rs:784-785`).

- Check `crates/platform/runtime/src/component_admission.rs:270-275`.
- Check `crates/catalog/model/src/component_library.rs:796-798`.
- Both now key on the registry. Both match on package at any version. The
  question is what kind of import this is, and kind does not change with
  version (`crates/platform/runtime/src/component_admission.rs:261-263`).

Search the tree for any remaining namespace test before you push the first
guest. **These classifiers get one case wrong.** That case is a seam whose
package namespace is neither `wasi` nor `wamn`. `wasmcloud:blobstore` is the
one landed example. MQTT was the expected second one until the owner withdrew
it on 2026-09-07.

**If you skip this step:** the seam admits in the registry and refuses at push.
The error names a dependency mismatch instead of the real cause.

### Step 9: Resolve a binding, then register the plugin

- Artifact: a `binding` module. It turns an authorized snapshot into a confined
  store handle (`caa88d9b`).
- Artifact: one line in `crates/platform/runtime/src/plugins/mod.rs:4`.

**If you skip the registration:** nothing implements the imported interface at
run time. A guest that admitted correctly then traps on its first call.

### Step 10: Add a per-effect span and a latency series

Artifact: one entry in
`crates/platform/runtime/src/plugins/effect_span.rs:161-163`. Blobstore's entry
names the metric and the unit. It breaks the series down by the effect's
operation.

**If you skip this step:** the seam is invisible in a trace. The composed proof
in step 12 cannot then show which node did the work.

### Step 11: Write the palette node that imports the capability

A capability is a guest import. It is not an operation. Wirings compose
operations. A capability therefore reaches a wiring through a small node that
imports it (`docs/poc/wms-prep-spec.md:156-158`).

- Artifact: the node's own world. It imports the capability and exports the
  node contract (`components/execution/blob-put/wit/world.wit:3-8`).
- Artifact: the node body.

Two rules from `blob-put` generalize. Both are already law.

- **The caller supplies any deterministic key. The node never generates one.**
  Redelivery must be an idempotent overwrite, never a second object
  (`components/execution/blob-put/src/lib.rs:3-13`,
  `docs/poc/wms-prep-spec.md:159-162`).
- **The contract has an `async func`, or it does not. In the first case the
  node must export the async node contract.** The component model forbids a
  synchronously lifted export from blocking on an async import. `blob-put`
  trapped on its first ever execution before this was fixed
  (`components/execution/blob-put/src/lib.rs:33-44`, ruled `wamn-362o.46`).

Ask the async question at step 2, while you read the contract. Do not leave it
to step 11.

### Step 12: Stand up disposable gate infrastructure and prove the seam released

These are two different things. The blobstore run needed both.

- `deploy/infra/minio.yaml:1` is install-once infrastructure for a real
  cluster. A journey does not use it.
- `819790ee` (`wamn-362o.26`) stood up a named disposable object store inside
  the WMS journey. It minted the credential. It created the container before
  any release existed. It removed the store by name in cleanup. The credential
  reaches the host as a mounted secret. The guest sees only a handle.

Then prove the capability in a released deployment, not only in a test
(`3bcbd39d`, `wamn-jpxo.1`). The journey pattern is
`docs/operations/build-and-test.md:1585-1620`.

**If you skip this step:** nothing measures the seam end to end. The
portfolio's rule then has nothing to check. That rule says an application
without a measurable gate is not built
(`docs/poc/poc-application-portfolio.md:58`).

## 3. The checklist

| # | Step | Artifact | What refuses after a skip |
|---|---|---|---|
| 1 | adopt or author the contract | a recorded decision | missing verbs found late |
| 2 | vendor the WIT, declare a host world | `runtime/wit/deps/…`, one `world` block | no world to bind against |
| 3 | ledger row | one row in the native-alignment ledger | standing trigger 7 |
| 4 | capability registry row | one `CapabilityRow`, plus the conformance set | admission fails closed by absence |
| 5 | connection vocabulary | type variant, descriptor, push mapping, bind coordinates | nothing binds |
| 6 | confinement and intake | `confinement.rs`, `intake.rs` | the ownership split never becomes a refusal |
| 7 | bindings, error map, bodies | `bindings.rs`, `wit_error.rs`, `host.rs` | a second error taxonomy |
| 8 | fix namespace classifiers | two call sites keyed on the registry | admits, then refuses at push |
| 9 | binding resolution, plugin registration | `binding.rs`, one line in `plugins/mod.rs` | traps on first call |
| 10 | span and latency series | one entry in `effect_span.rs` | invisible in a trace |
| 11 | palette node | node world and body | no wiring can reach the capability |
| 12 | disposable gate infra, released proof | journey container, journey assertion | no measurable gate |

## 4. What this template does not decide

- **Whether a seam needs a guest-visible interface at all.** The platform can
  use a capability on its own behalf. Such a seam is host-side only, with no
  WIT and no registry row. This is a design choice, and it stays open in
  general. The owner settled the MQTT case on 2026-09-07: a capability a guest
  RECEIVES from needs no guest-visible interface, because the platform consumes
  and delivers (`wamn-7tva.1`, `wamn-7tva.2`).
- **The posture of a new row.** `Ambient` or `Effect` is a security judgment
  the owner makes. The registry only records it.
- **Which verbs to refuse.** Step 3 records refusals. It does not choose them.
- **Bindings at deploy.** No host, no URL, no environment name and no secret
  belongs in any artifact this template produces. Those are bindings at deploy
  (`docs/experiments/agent-authoring/protocol.md:187-188`).

## 5. Known gaps in this template

Each of these needs a bead. This lane cannot create beads, so **the integrator
must file them.**

1. **Steps 3 and 4 have no automated ordering check.** A registry row that
   lands without a ledger row fails nothing. The conformance suite pins the row
   set (`tests/conformance/tests/capability_registry.rs:136-151`). It does not
   read the ledger.
2. **Step 8 has no test that finds a new namespace classifier.** The two known
   call sites are fixed. A third gets found the same way the second was, which
   is by a live refusal.
3. **This document has no owner ruling yet.** It is a derivation from one
   execution. The second execution either proves it or corrects it. That
   correction is what makes it a template rather than a description.
