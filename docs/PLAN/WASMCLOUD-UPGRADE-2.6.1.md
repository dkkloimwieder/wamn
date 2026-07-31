---
status: draft
genre: upgrade delta
date: 2026-07-30
amends: docs/PLAN/WASMCLOUD-UPGRADE-2.6.0.md
---

# wasmCloud v2.6.1 upgrade delta

Retarget the fork upgrade to upstream **v2.6.1**, peeled commit
`df8a8bcd69adc9c23ded842e504071a5272d04ed`, on branch `wamn/2.6.1`.
This document amends the v2.6.0 upgrade plan; every requirement, gate, exit
condition, and exclusion not changed below still stands.

## Upstream delta

v2.6.1 adds three commits after the v2.6.0 target
`9bf8e97b28bfbc26d629aca1d5c4d8f14a8cfe6b`:

| Commit | Delta |
|---|---|
| `dd8eccedd9eb96458fa6443fb940ba4bfb57667a` | Rename `HttpServer` to `Ingress` and `HttpServerBuilder` to `IngressBuilder`. |
| `9c7aa1fa9dbe6ab243797a655e74b6049aaa8193` | Correct and pluralize the IP-name-lookup option across public and internal representations. |
| `df8a8bcd69adc9c23ded842e504071a5272d04ed` | Release-version bump for v2.6.1. |

The two substantive commits are renames. They do not satisfy a carried-policy
exit condition or change the intended behavior of the re-port.

## Canonical names

The lookup option is **not** `AllowIPNameLookup` or JSON/YAML
`allowIpNameLookup`. Its canonical forms are:

- public field: `AllowedIPNameLookups`
- JSON/YAML: `allowedIpNameLookups`
- Rust: `allowed_ip_name_lookups`
- internal lookup state: `allowed_ip_name_lookups`

Use those names in the gate list, the post-upgrade adoption milestone, code,
manifests, fixtures, and ledger. The outbound trace-injection re-port targets
`Ingress` / `IngressBuilder`, not the former `HttpServer` names.

## Dependencies

The dependency target is unchanged from the v2.6.0 plan:

- Wasmtime family: `47.0.1`
- `async-nats`: `0.49.1`
- `rust-version`: `1.94.0`

No dependency realignment beyond the v2.6.0 plan is part of this delta.

## Carried-policy disposition

All seven carried seams remain required, and all seven upstream exit conditions
remain unsatisfied:

1. epoch-deadline enforcement
2. memory limiting
3. outbound W3C trace injection, re-ported onto `Ingress`
4. raw TCP denial, reconciled with `AllowedIPNameLookups`
5. raw UDP and `UdpBind` denial, reconciled with `AllowedIPNameLookups`
6. limiter accessors
7. `wamn.api.requests` request counting, re-ported onto `Ingress`

Re-port and prove each seam independently. The lookup rename does not authorize
retiring either raw-socket restriction, and the Ingress rename does not replace
the trace or request-count behavior.

## Tag verification

Verify the upstream target from Git refs, including the peeled annotated tag:

```sh
git ls-remote --tags https://github.com/wasmCloud/wasmCloud.git \
  'refs/tags/runtime-operator/v2.6.1*'
```

Do not use the GitHub releases page as the version authority. It currently shows
v2.4.0 as the latest release and omits the v2.5.x and v2.6.x tags.
