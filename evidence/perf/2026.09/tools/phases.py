#!/usr/bin/env python3
"""phases.py <trace.json>... : decompose a hot request into who owns each ms.

Uses the fork's http.* spans as boundaries. Per trace and averaged:
  fork host calls   sum of http.* leaves (not handle/await_head)
  fork glue         (handle_http_request - http.handle) - fork host calls
  flow guest own    http.handle minus the union of the in-tree top-level spans
  route plugin      route.match + validate_input + permit + router.resolve + the
                    jetstream publish outside component.invoke
  authenticate      route.authenticate, split into named children and its own
  component.invoke  split into named children and its own (driver + the
                    data-access guest's own path)
  residue2          total - union(every span except the four waiters), the
                    number the earlier reports called the unspanned residue
"""
import json
import sys

WAITERS = {"handle_http_request", "invoke_component_handler", "http.handle", "http.await_head"}
FORK_LEAVES = {
    "http.route", "http.lookup_service", "http.lookup_workload", "http.new_store",
    "http.incoming_request", "http.instantiate", "http.store_drop",
}
TOP_IN_TREE = {
    "wamn.route.match", "wamn.route.authenticate", "wamn.route.validate_input",
    "wamn.route.permit", "wamn.router.resolve", "wamn.component.invoke", "wamn.jetstream",
}
AUTH_CHILDREN = {"wamn.auth.pat", "wamn.auth.roles", "wamn.auth.permissions"}
PERM_CHILDREN = {"wamn.postgres.acquire", "wamn.auth.perm.query"}
INVOKE_CHILDREN = {
    "wamn.component.cache_hit", "wamn.component.link", "wamn.component.linker_setup",
    "wamn.component.instantiate", "wamn.postgres", "wamn.jetstream",
}


def spans(path):
    d = json.load(open(path))
    out = []
    for b in d.get("batches", []):
        for ss in b.get("scopeSpans", []):
            for s in ss.get("spans", []):
                out.append((s["name"], int(s["startTimeUnixNano"]), int(s["endTimeUnixNano"])))
    return out


def union(intervals):
    total = 0
    cur_s = cur_e = None
    for s, e in sorted(intervals):
        if cur_e is None or s > cur_e:
            if cur_e is not None:
                total += cur_e - cur_s
            cur_s, cur_e = s, e
        else:
            cur_e = max(cur_e, e)
    if cur_e is not None:
        total += cur_e - cur_s
    return total


def within(sp, lo, hi, names):
    return [(max(s, lo), min(e, hi)) for n, s, e in sp if n in names and e > lo and s < hi]


def one(sp, name):
    m = [x for x in sp if x[0] == name]
    return m[0] if m else None


def analyse(path):
    sp = spans(path)
    root = one(sp, "handle_http_request")
    r0, r1 = root[1], root[2]
    total = r1 - r0
    handle = one(sp, "http.handle")
    h0, h1 = handle[1], handle[2]
    fork_calls = sum(e - s for n, s, e in sp if n in FORK_LEAVES)
    fork_glue = (total - (h1 - h0)) - fork_calls
    inv = one(sp, "wamn.component.invoke")
    # the jetstream publish outside invoke belongs to the route plugin
    top = []
    for n, s, e in sp:
        if n in TOP_IN_TREE and e > h0 and s < h1:
            if n == "wamn.jetstream" and inv and s >= inv[1] and e <= inv[2] + 2_000_000:
                continue
            top.append((max(s, h0), min(e, h1)))
    flow_guest = (h1 - h0) - union(top)
    auth = one(sp, "wamn.route.authenticate")
    auth_total = auth[2] - auth[1]
    auth_named = union(within(sp, auth[1], auth[2], AUTH_CHILDREN))
    perm = one(sp, "wamn.auth.permissions")
    perm_total = perm[2] - perm[1]
    perm_named = union(within(sp, perm[1], perm[2], PERM_CHILDREN))
    route_plugin = sum(
        e - s for n, s, e in sp
        if n in {"wamn.route.match", "wamn.route.validate_input", "wamn.route.permit", "wamn.router.resolve"}
    ) + sum(e - s for n, s, e in sp if n == "wamn.jetstream" and not (inv and s >= inv[1] and e <= inv[2] + 2_000_000))
    inv_total = inv[2] - inv[1]
    inv_named = union(within(sp, inv[1], inv[2], INVOKE_CHILDREN))
    residue2 = total - union([(max(s, r0), min(e, r1)) for n, s, e in sp if n not in WAITERS and e > r0 and s < r1])
    return {
        "total": total,
        "fork host calls": fork_calls,
        "fork glue": fork_glue,
        "flow guest own path": flow_guest,
        "route plugin spans": route_plugin,
        "authenticate named": auth_named,
        "authenticate own": auth_total - auth_named,
        "  permissions own (inside authenticate own)": perm_total - perm_named,
        "component.invoke named": inv_named,
        "component.invoke own (driver + data guest)": inv_total - inv_named,
        "residue2 (old method)": residue2,
        "http.handle": h1 - h0,
    }


def main(paths):
    rows = [analyse(p) for p in paths]
    keys = list(rows[0].keys())
    print(f"{'ms/req':44s}" + "".join(f"{p.split('-')[-1][-4:]:>9s}" for p in paths) + "      avg")
    for k in keys:
        vals = [r[k] / 1e6 for r in rows]
        print(f"{k:44s}" + "".join(f"{v:9.3f}" for v in vals) + f"{sum(vals)/len(vals):9.3f}")
    parts = ["fork host calls", "fork glue", "flow guest own path", "route plugin spans",
             "authenticate named", "authenticate own", "component.invoke named",
             "component.invoke own (driver + data guest)"]
    for r in rows:
        assert abs(sum(r[p] for p in parts) - r["total"]) < 2_000, "decomposition does not sum to the total"
    print("decomposition sums to the total on every trace")


if __name__ == "__main__":
    main(sys.argv[1:])
