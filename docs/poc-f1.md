# POC-F1: callable `receipt-received`

F1 is the synchronous request flow defined by `docs/POC-PLAN.md` r6 and
`docs/FLOW-SPEC.md` rev18. The former embedded `poc-webhook-f1` executor was
deleted by `wamn-5wd1.57`; generic `flow-http` ingress and the production
invocation provider now own transport and execution.

## Runtime shape

`deploy/poc/f1-flow.json` is the canonical graph:

```text
request
  → normalize-receipt
  → resolve-and-persist
  → references-valid
      false → invalid-reference
      true  → evaluate-specs
            → create-holds
            → shape-response
            → respond

normalize-receipt.error → invalid-input
```

`normalize-receipt` and `evaluate-specs` are independently deployable pure
custom-node components. The graph names their supplied node types directly;
it does not use the retired `type=custom` plus `config.manifest` indirection.
The immutable artifact identity therefore pins both interfaces and component
digests.

Both PostgreSQL nodes run in query mode, require the POC environment's explicit
`raw_sql_enabled` grant, and use natural-key conflict handling plus deterministic
read-back. `resolve-and-persist` returns line rows in `line_no` order.
`create-holds` reads back the unique `(tenant_id, line_id)` rows and returns
holds in line order. Repeating either CTE after a commit-before-attempt-success
fault yields byte-identical ordered rows.

## Exposure

`deploy/poc/f1-http-attachment.json` defines the callable exposure:

- `POST /receipts`;
- whole request body mapped to the request entry;
- `erp-api-keys` selected auth source;
- required idempotency at the ingress contract;
- a 30-second response deadline within a 60-second run deadline.

Schema, auth, RawSql, and idempotency refusals occur before admission and create
no run. Unknown supplier, site, or material references are authored business
failures: the admitted run terminates failed with caller status 400.

## Proof

The package and exact-image recipes are in `docs/build-and-test.md` under
`wamn-5wd1.57`. `tests/system/src/callable_f1.rs` pins:

- immutable graph, supplied-component, source, attachment, and release identity;
- direct custom-node types and explicit RawSql grant;
- malformed/bad-key/auth/config no-run refusals and authored 400 failure;
- independent committed-but-unrecorded faults for both CTEs;
- byte-identical ordered rows, shaped response, outcome hash, and logical rows;
- absence of the retired webhook from the component workspace, image, and
  architecture inventory.

The serial Wave-1 campaign in `wamn-5wd1.9` owns the from-zero composed-cluster
T0/T-CTX/T-NR/T1/T3 proof across F0, F1, and F3.
