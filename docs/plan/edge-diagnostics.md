# Edge diagnostics

Updated through: 2026-10-01, `main` at `43497e343`. The owner accepted option A of sections 4.1 to 4.3 on 2026-10-01, with the fixed loopback listener of section 4.2.

## 1. Goal

An operator of a `wamn-edge` box reads four facts while the edge runs:

- the dropped frames of the device loop: empty, not UTF-8, or longer than `max_frame`;
- the credential failures of the forward, which are 401 or 403 answers, and whether the forward stopped at `credential_bound`;
- the refused samples, with the platform's reason;
- the pending sample count.

Today the edge serves only the routes of its release, and the operator reads none of these facts on a running box. This epic adds one read surface on the edge and one view of it. The Epic 19 closeout named it as the next epic (`wamn-e5in.10`).

## 2. Fixed rules

- The edge links nothing new from the list that `docs/plan/edge.md` section 2 refuses: no `wamn-runtime`, Postgres, NATS, OCI or OTLP gRPC. The dependency test stays as it is.
- The release routes keep one path matcher and one lowering, the `http-route` guest (`docs/plan/edge.md` section 4.3). The read surface does not add a second matcher to that ingress.
- The read surface reads. Resolving a refused sample stays the `wamn-edge samples resolve` command on a stopped edge.
- The surface reports what the process holds. It adds no new stored state for the four facts.

## 3. Current state

Measured on `main` at `43497e343` on 2026-10-01.

| Fact | Today |
| --- | --- |
| Dropped frames | `DeviceLoop::dropped_frames()` (`services/edge/src/device.rs:40`) reads an in-memory `AtomicU64` since start. Only tests call it. |
| Credential failures | `Forward::credential_failures()` (`services/edge/src/forward.rs:69`) reads an in-memory `AtomicU64` since start. Only tests call it. |
| Forward stopped | After `credential_bound` credential passes in a row, `run` logs an error and waits for the stop signal (`forward.rs:214-224`). Nothing outside the task can read that it stopped. |
| Pending samples | `SampleStore::pending(limit)` (`services/edge/src/samples.rs:116`) returns up to `limit` rows where `forwarded_at` and `refused_at` are null. No count query exists. |
| Refused samples | `SampleStore::refused_samples()` (`samples.rs:232`) returns every refused sample that no operator resolved. Only `wamn-edge samples list` calls it (`services/edge/src/refusals.rs`), and it must open the run-state file, which a running edge holds. |
| Access to the host | `EdgeHost` holds the device loop, the forward and the sample store (`services/edge/src/serve.rs:47-73`). |
| Ingress | wash-runtime's `Ingress` routes each request to a workload through its `Router` trait. It has no native handler. The edge's ingress serves the route guest only. |
| Native listener precedent | wash-runtime's probe listener serves `/livez` and `/readyz` from the host on a port of its own (`wash_runtime::host::probes`). `services/host` binds it at `--probe-addr`. |
| Edge documentation | The behavior of the edge is still in `docs/plan/edge.md`. No page under `docs/architecture` describes `wamn-edge`. |

## 4. Design

### 4.1 The read surface

| Option | Rule | Cost |
| --- | --- | --- |
| A (accepted) | The edge host serves `GET /status` on a listener of its own, as the probe listener does. The listener binds `127.0.0.1:8081` (section 4.2). The answer is one JSON object built from `EdgeHost`. | One small native HTTP handler in `services/edge`. |
| B | A release operation reads the facts through a new host import. | A WIT interface for host facts, and a release that must import it. |
| C | The forward and the device loop write the counters to SQLite, and `wamn-edge status` reads them on a stopped edge. | It does not serve a running box, which is the goal. |

The answer of option A:

```json
{
  "started_at": "2026-10-01T12:00:00Z",
  "device": {"dropped_frames": 0},
  "forward": {"credential_failures": 0, "stopped": false},
  "samples": {"pending": 3, "refused": [{"sample_key": "…", "captured_at": "…", "reason": "…"}]}
}
```

`device` and `forward` are null when the configuration names no device or no forward. `stopped` is true after the forward stops at `credential_bound`. Each refused sample carries its key, its capture time and the platform's reason. The answer carries no sample body. The counters count since `started_at`, because they live in memory. The forward gains a shared `stopped` flag that `run` sets where it logs the stop. The sample store gains a count of pending samples.

### 4.2 Who can read it

| Option | Rule | Cost |
| --- | --- | --- |
| A (accepted) | The listener binds `127.0.0.1:8081`, and no configuration key binds it elsewhere. It checks no credential, as the probe listener checks none. | Anyone on the box reads the counts and the reasons of refused samples. Nobody off the box reads them. |
| B | The listener verifies a session as a local route does, through `EdgeAuthenticator`, and requires one permission in `grants.json`. | A permission name, and a session on the box for the operator. |

### 4.3 The view

| Option | Rule | Cost |
| --- | --- | --- |
| A (accepted) | `wamn-edge status` prints the answer of a running edge as text, in the form `samples list` uses. | No web page. |
| B | A page in a new web package reads `/status`. | A web package, and a host that serves its files, which the box does not have. |
| C | A screen of the terminal client in `crates/client/tui`. | The terminal client learns a second contract beside the release contract. |

## 5. Issues

One chain, the routes agent, after acceptance. Each issue lands with its tests.

1. The read surface: the `stopped` flag of the forward, the pending count, the listener and its answer. Tests: the answer before and after a dropped frame, a credential failure, the stop at `credential_bound`, a refused sample, and a pending sample. The listener binds the loopback address only.
2. The view of section 4.3.
3. Closeout. The edge's status read moves into the documentation that holds the edge's behavior, and `docs/operations` states the command. Workspace test run.

## 6. Out of scope

- Resolving a refused sample on a running edge.
- Counters that survive a restart.
- A configuration key that binds the status listener to another address. Such a key would put the facts on the network without a credential.
- A credential on the status listener.
- Sample bodies in the status answer.
- A push of the facts to the platform.
- Moving the rest of `docs/plan/edge.md` into `docs/architecture`.
