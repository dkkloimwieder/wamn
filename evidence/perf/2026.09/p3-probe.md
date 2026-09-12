# p3 probe — does `wasi:http@0.3` serve `flow-http`, and what does it buy

A throwaway, per the directive on `wamn-0h0g.17.26`: measure, report, migrate
nothing until ruled. The throwaway branch is `perf/p3-probe` in the perf lane
and never lands; this report does.

| | |
|---|---|
| bead | `wamn-0h0g.17.26` |
| baseline | `1c-c-statement-sets/` (p2, `113691fa`) |
| p3 source | throwaway `8ffc9ec7` on `perf/p3-probe` (eight commits; never lands) |
| load at launch | 3.72 (p3), against 6.25 for the p2 baseline run |

## 1. Features — nothing to add

The host already links both surfaces on any component that uses `wasi:http`:
`wasmtime_wasi_http::p2::add_only_http_to_linker_async` and
`wasmtime_wasi_http::p3::add_to_linker`, side by side, and `add_wasi_to_linker`
carries `wasmtime_wasi::p3::{cli,clocks,filesystem,random}` beside p2. The fork
pins `wasmtime-wasi = { features = ["p3"] }`, `wasmtime-wasi-http = { features =
["p2", "p3", "component-model-async"] }` and `wasmtime = { features =
["component-model-async", ...] }`, so those features reach our build by
unification whatever the workspace's `default-features = false` pin says about
wasmtime itself. Dispatch is by shape: `targets_wasip3_http` (any
`wasi:http@0.3` import or export) selects `handle_component_request_p3`. The
feature allowlist review has nothing to review.

## 2. The guest — it componentizes; the registry says what it costs

`flow-http` rebuilt to export `wasi:http/handler@0.3.0`:

- **wit-bindgen.** The components workspace pins `wit-bindgen 0.61` with
  `default-features = false, features = ["macros", "realloc"]`; a p3 export needs
  `async` (the async ABI) and `async-spawn` (to stream the response body after
  `handle` returns -- the host reads the body only after it has the response,
  so writing before returning would deadlock). Selective async keeps the
  `wamn:*` imports synchronous: `async: ["export:wasi:http/handler@0.3.0#handle"]`.
- **Body.** p3 has no `wasi:io` streams; the request body is a component-model
  `stream<u8>` read with `Request::consume_body`, the response body a stream the
  guest writes from a spawned task. The probe pre-reads the request body to one
  byte past the adapter's limit and hands the sync adapter a buffered reader, so
  the adapter's bounded-read refusal decides exactly as before.
- **Vendored WIT.** The p3 `http.wit` from `wasmtime-wasi-http 47.0.3` bundles
  `world service` and `world middleware`, which pull `wasi:cli@0.3.0`,
  `wasi:filesystem@0.3.0`, `wasi:sockets@0.3.0` and `wasi:random@0.3.0`.
  Vendored whole, the capability registry refused it exactly as designed:
  `wasi:cli is vendored in the tree but neither registered nor listed as
  deliberately absent; a new WASI dependency needs a ruling, not silence`.
  Trimmed to the `types`, `handler` and `client` interfaces plus
  `wasi:clocks@0.3.0` (which `types` uses for `duration`), all three registry
  tests pass: `wasi:http` is deliberately absent by rule and the clocks row
  tolerates the second version.

The built component, 238,291 bytes, valid:

```
import wamn:flow-http-routing/routing@0.1.0;
import wamn:router-delivery/delivery@0.1.0;
import wasi:random/random@0.2.12;
import wasi:http/types@0.3.0;
import wasi:clocks/monotonic-clock@0.2.9;   (+ the wasip2 std surface: wasi:io, wasi:cli @0.2.9, unchanged from p2)
export wasi:http/handler@0.3.0;
```

`flow-http` is not virtualized (`tools/component-virtualization.json` names only
the three data guests), so "virtualize" does not apply to it; the std surface it
imports is the same one the p2 build imports. Admission is the platform push
path, not `analyze_tenant`, so the tenant registry never sees it either way.

**The cost nobody asked about: every other guest's digest moved.** The first
journey on the throwaway refused at the release mint --
`release-manifest-mint-refused (operation-dependency): component dependency
wamn_receiving@1.0.0 digest sha256:43fcb1f1... has no exact admitted fact`.
`tools/build-components` builds the components workspace in one cargo
invocation, so the `async` and `async-spawn` features one guest needs unify
into `wit-bindgen` for every guest in it; the virtualized `receiving.wasm`
built minutes earlier from the same tree at `43fcb1f1...` came out at
`3203875c...`. A p3 `flow-http` in the shared workspace is therefore a remint
of every std guest's digest and its six tree pins, exactly as the release
profile was -- or a build split that keeps `flow-http`'s feature set out of the
data guests' unification. The probe reminted the pins on the throwaway to get
its trace; a migration would have to choose. The remint then tripped the
second guard exactly as the release-profile remint did: `wamn.json` carries the
dependency digest, `generated/platform-policy/data-access.json` carries a hash
OF `wamn.json`, and the journey refused at convergence with
`package-data-access-manifest-drift` until that hash was regenerated. Two
guards, two refusals, both correct; a p3 `flow-http` in the shared workspace
costs both remints on every guest-feature change.

**Two more things stand in front of the cluster, neither in the host.** The
journey's pre-cluster proof, `production_two_package_release_serves_all_thirteen_pat_routes`,
instantiates the shipped `flow-http` in-process through
`wasmtime_wasi_http::p2::bindings::Proxy` and drives it with
`wasi_http_incoming_handler().call_handle(incoming, out)`: a p2 world by
construction, and it refused the p3 guest at the linker (`component imports
instance wasi:http/types@0.3.0, but a matching implementation was not found`).
It also mints the route-caller PAT the cluster stage consumes, so it cannot be
skipped; the throwaway fed it the p2 guest built from `origin/main` and pushed
the p3 guest to the cluster. Porting it is the fork's `http_p3.rs` again --
`Request::from_http`, `Service::handle` under `run_concurrent`,
`Response::into_http` and the body plumbing -- and the same p2 proxy pattern
lives in `virtualized_std_guest.rs`, `router_tap_live.rs` and the materializer
test. Second, the journey's workload manifest names the host interface
`wasi:http` with `interfaces: [incoming-handler]`; the host accepts
`incoming-handler` or `handler` (`is_incoming_http_handler`), so a p3 workload
declares `[handler]`, and every manifest that names the p2 interface moves with
it -- including the journey's renderer, which anchors on the literal
`interfaces: [incoming-handler]` line and refused a manifest that lacked it; the
throwaway substitutes the name after the render.

**The blocker in the toolchain: `wash push` 0.40 cannot ship the artifact.**
With the proof fed the p2 guest, the journey's next step, `wash push` of the p3
`flow-http`, answered `{"success": false, "error": "Unsupported artifact
type"}`. The installed `/usr/local/bin/wash` is v0.40.0 and bundles wasmparser
0.218 through 0.224, which predate the component-model async ABI the p3
handler export lifts through; a component it cannot parse is not a component
to it. The host pulls either the oci-wasm `application/wasm` layer or the old
wasmcloud one, so the repo's own `wamn-ctl push-component`
(`application/vnd.wamn.component.v1+wasm`) is not a substitute. The sibling
fork checkout carries the `wash` CLI that pairs with this host (`wash 2.8.0`,
`wit-component 0.254`), built here in debug for the throwaway push. It pushed
the p3 guest on the first try (digest `ae986310...`), through a shim that maps
0.40's `wash push --insecure --allow-latest --output json` onto the fork's
`wash -o json oci push --insecure --user --password` and lifts the receipt's
`digest` out of the `data` object the fork nests it under. A migration replaces
the installed CLI and re-reads that receipt; both are journey-tooling changes,
neither is host work.

**The host serves it, and the first thing the guest did was refuse.** With the
push and the manifest in place the p3 `flow-http` reached Ready, and the cold
arm got an answer in 5-7 ms on every attempt: our own
`{"error":{"code":"invalid-authority"}}`, 147 times. p2's
`IncomingRequest::authority()` returned the Host header; p3's
`Request::get_authority()` carries an authority only when the client sent an
absolute-form target, and an origin-form request keeps it in the `host` header.
The guest's route match runs on the authority, so under p3 it saw an empty
string. One fallback in `request_head` -- the host header when the request
carries none -- and the route matches. That is a real semantic difference the
migration has to own, and the adversarial route tests (`tests/adversarial.rs`)
never exercised it because they drive `handle_request` below the WASI shell.

## 3. The trace

The eighth run served all three arms on the p3 path: cold 105.9 ms, restart
recovery 38 s, steady pass, `overhead_ratio=6.87` against the same ceiling of
12. The journey's single steady sample read 42 ms with every phase inflated
alike (instantiate 3.8, linker_setup 2.9), a load spike it happened to land on;
the four hot samples below are the measurement.

**The unspanned residue**, per hot trace: `handle_http_request` minus the
union of every named span's interval inside it. Load-tolerant the same way the
ratio is: a share, not a millisecond.

| | p2 (`1c-c`, load 6.25) | **p3 (load 3.72)** |
|---|---:|---:|
| `handle_http_request` | 15.725 | 11.864 |
| covered by named spans | 10.569 | 7.758 |
| **unspanned residue** | **5.156** | **4.106** |
| residue share of the request | **32.8 %** | **34.6 %** |
| `wamn.component.invoke` | 7.590 | 5.350 |
| `wamn.route.authenticate` | 2.246 | 1.811 |
| `wamn.postgres` | 2.480 | 1.319 |
| `wamn.component.instantiate` | 1.631 | 1.254 |
| `wamn.component.linker_setup` | 0.827 | 0.719 |

Every row dropped by about the same quarter, the residue included -- the
Postgres and instantiate rows have nothing to do with the HTTP path and dropped
with it. That is the lighter machine, not the protocol. **The residue's share
of the request did not move: 32.8 % under p2, 34.6 % under p3.** Whatever the
4-5 ms of unspanned time is made of, p3's body streams did not take it.

## Verdict

**p3 serves, and it does not buy the residue.** The host needed nothing: it
already links both surfaces, dispatches on the export, and answered the p3
guest on the first request that reached it. Everything that stood in the way
was ours -- the shared-workspace digest remint and its manifest-hash pin, the
p2-bound in-process proofs, the p2 interface name in every workload manifest,
a `wash` CLI two major versions behind the host, and a real semantic
difference in where the authority lives -- and the trace at the end of it puts
the unspanned share at 34.6 % against 32.8 %.

Per the directive: it serves, so it is a lane by the letter; on this evidence
it is not a lane worth cutting for the residue, because the residue is not
where p3 would find it. The ~3 ms of `.17.16` needs a span, not a protocol:
the next probe is instrumentation inside `invoke_component_handler` and the
`flow-http` guest's own path, on p2, where the 4-5 ms actually sits. The p3
migration costs above are recorded here for the day something else wants p3.
