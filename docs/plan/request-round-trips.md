# Request round-trips

Updated through: 2026-10-16, `main` at `77a7587b6`. Epic `wamn-stxy9`.

## Law

- The platform adds zero Postgres round-trips to a request.
- The host answers auth from its memory.
- The application's statements run in one transaction on one connection, with the session claims pipelined in front.
- Test: `pg_stat_statements` calls per get request = 1.

## Measured hops

Platform 0.1.19, concurrency 16, host p50 in ms. The p50 values do not add up to the total. Source: [benchmark](../operations/benchmark.md#named-parts).

| Part | get | update |
| --- | --- | --- |
| wash-runtime invoke (no span) | 6.15 | 5.50 |
| record_actor_labels query | 4.01 | 3.87 |
| identity | 2.20 | 1.97 |
| permission query | 1.82 | 1.66 |
| principal claims query | 1.47 | 1.91 |
| bind claims | | 1.74 |
| session settings | 1.75 | |
| application statement | 1.73 | 6.79 |
| write-log claim | | 3.20 |
| item commit | | 8.42 |
| write-log finish | | 4.60 |
| total `invoke_component_handler` | 22.19 | 47.13 |

## Round-trip map

SYS is the one client on `wamn_system` (`services/host/src/host.rs:990-1012`). PLAT is the `CallableHttp` pool on the environment database (`host.rs:624-626`). GUEST is the per-tenant `GuestSql` pool on the environment database (`crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:350`). Paths below `claims.rs`, `transactions.rs`, `resources.rs`, `write_log.rs` and `operation_transaction.rs` are in `crates/platform/runtime/src/plugins/wamn_postgres/`.

| # | Hop | Code | Span | Connection | get ms | update ms | Deleted by |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | environment status | `crates/platform/engine/src/flow_http_routing.rs:705`, `claims.rs:36`, `claims.rs:1969` | none | GUEST | inside wash-runtime invoke | inside wash-runtime invoke | no issue |
| 2 | PAT lookup by prefix | `crates/execution/host/src/route_authentication.rs:330`, `crates/identity/platform/src/lib.rs:98`, `lib.rs:965` | `wamn.auth.identity` | SYS | 2.20 | 1.97 | `wamn-stxy9.2` |
| 3 | session still active (session only) | `crates/identity/platform/src/session_token.rs:140`, `route_authentication.rs:249` | none | SYS | not in the PAT run | not in the PAT run | `wamn-stxy9.2` |
| 4 | permission rows | `claims.rs:59` and `claims.rs:1901` (PAT), `claims.rs:41` and `claims.rs:1799` (session) | `wamn.auth.perm.query` | PLAT | 1.82 | 1.66 | `wamn-stxy9.2` |
| 5 | principal exists | `claims.rs:54`, `claims.rs:1449-1487` | `wamn.claims.principal.query` | GUEST | 1.47 | 1.91 | `wamn-stxy9.2` |
| 6 | autocommit session settings | `transactions.rs:108`, `transactions.rs:429` | `wamn.postgres.session_settings` | GUEST | 1.75 | | `wamn-stxy9.3` |
| 7 | BEGIN and claims | `transactions.rs:113`, `transactions.rs:245`, `crates/platform/engine/src/operation/intent.rs:227` | `wamn.postgres.bind_claims` | GUEST | | 1.74 | `wamn-stxy9.3` |
| 8 | write-log claim | `write_log.rs:35`, `intent.rs:368` | `wamn.write_log.claim` | GUEST, item transaction | | 3.20 | `wamn-stxy9.3` |
| 9 | application statement | `resources.rs:347` (get), `resources.rs:1407` (update) | `wamn.postgres.statement` | GUEST | 1.73 | 6.79 | stays |
| 10 | write-log finish | `write_log.rs:45`, `intent.rs:439` | `wamn.write_log.finish` | GUEST, item transaction | | 4.60 | `wamn-stxy9.3` |
| 11 | COMMIT | `operation_transaction.rs:77`, `intent.rs:292` | `wamn.operation.commit` | GUEST, item transaction | | 8.42 | `wamn-stxy9.3` |
| 12 | actor labels | `claims.rs:1835`, `crates/execution/host/src/router_delivery.rs:486`, `router_delivery.rs:798` | none, inside `wamn.route.deliver` | PLAT | 4.01 | 3.87 | `wamn-stxy9.1` |
| 13 | model versions (lists only) | `router_delivery.rs:358-368` | none | PLAT | not in the run | not in the run | no issue |

A get makes 7 round-trips: hops 1, 2, 4, 5, 6, 9 and 12. An update makes 10: hops 1, 2, 4, 5, 7, 8, 9, 10, 11 and 12.

## Findings

- No issue deletes hops 1 and 13. Both are platform trips.
- The update already runs hops 7 to 11 in one transaction on one connection, with one commit (`intent.rs:176-292`). The guest issues hop 9 during the call, between hop 8 and hop 10.
- The host verifies the session signature without Postgres (`crates/identity/session/src/verifier.rs:74-101`). Hop 3 is the session path's only identity trip.
- No code publishes on NATS at grant or revoke, and the host subscribes to no control subject (`services/ctl/src/role_verbs.rs:65-75`, `crates/control/lib/src/user_roles.rs:101-131`, `crates/execution/host/src/application_route.rs:176-217`).
- Hop 12 feeds `delivery-report.actor-labels` in the versioned WIT package `wamn:router-delivery@0.3` (`crates/execution/host/wit/deps/wamn-router-delivery-0.3/package.wit:135`). The http-route guest sets the `wamn-actor-labels` header from it (`apps/platform/ingress/http-route/src/guest.rs:428-435`). The terminal and the TUI read it (`crates/client/terminal/src/operator.rs:71-78`, `crates/client/tui/src/screen.rs:207-428`).
- Hops 1, 8, 10 and 12 send unprepared text, which costs a parse on each call.

## Issues

1. `wamn-stxy9.1`: delete `record_actor_labels` from the request path. Actor names come from the application's own SQL.
2. `wamn-stxy9.2`: auth in host memory. The host verifies the token without Postgres. An in-host map of (tenant, role, operation) to held fills by one query on a miss, which also replaces hop 5. A NATS message from the control host on grant or revoke drops entries, with a 60 s TTL as the backstop.
3. `wamn-stxy9.3`: one trip per request. The SET LOCAL claims are pipelined with the application statements. A command runs the claim row, the statements, the write-log row and the commit in one transaction.

At each merge, rerun get c16 and update c16 for 60 s, and record the calls per request from `pg_stat_statements` beside the span table. Target: get and update at 1 Postgres round-trip, host p50 under 10 ms.
