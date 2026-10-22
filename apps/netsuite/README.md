# NetSuite

This application composes `wamn_integration` and owns no domain tables.
The wirings implement every NetSuite request with JSONata and the platform HTTP nodes.
The Rust component only forwards `setting.read` and `setting.write` to the composed base.
It imports existing interfaces and contains no NetSuite request logic.
The package pins both base components by version and digest.

## Operations

Each operation is an executable wiring in `publication/wirings`.
The operation names below map to filenames with dots replaced by underscores.
Publish these wirings with the two workflow wirings.

| Operation | Input | Node parameters | Request |
| --- | --- | --- | --- |
| `record.get` | `{"id":"42"}` | `map.params.parameters.record_type` | GET `/services/rest/record/v1/<type>/<id>` |
| `record.create` | `{"body":{"entityId":"Acme"}}` | `record_type`, optional `external_id_input_pointer` | POST `/services/rest/record/v1/<type>` |
| `record.update` | `{"id":"42","body":{"entityId":"Acme"}}` | `record_type` | PATCH `/services/rest/record/v1/<type>/<id>` |
| `suiteql.query` | `{"q":"SELECT id, entityid FROM customer ORDER BY id"}` | Page limit in `page.params.path-and-query` | POST `/services/rest/query/v1/suiteql` |
| `restlet.call` | `{"body":{"message":"hello"}}` | `map.params.parameters.script` and `deploy` | POST `/app/site/hosting/restlet.nl?script=<script>&deploy=<deploy>` |

The record type defaults to `customer` in each mapping node.
The RESTlet parameters are examples. Replace them with the script and deployment IDs for the account.
Identifiers use percent encoding before they enter a path or query parameter.

For `record.create`, set `map.params.parameters.external_id_input_pointer` to a JSON pointer into the input, for example `/body/externalId`.
A JSON pointer selects a value inside JSON.
The pointer must select a nonempty string.
The mapping then selects the fixed PUT node and sends `/services/rest/record/v1/<type>/eid:<encoded-key>`.
Without the pointer, the mapping selects the fixed POST node.
Neither HTTP node takes its method from input.

SuiteQL sends `{"q":...}`, `Prefer: transient`, and offset and limit query parameters.
The page node reads `/items` and stops when `/hasMore` is false.
Queries must order by the unique `id` key.
The standalone query mapping refuses text without `ORDER BY id`.
The caller must keep that order as the query's actual final ordering clause.

## Pull and push

The minute schedule runs `netsuite_pull` with its due time.
The first mapping places that time in the optional `carry` field of `setting.read`.
The window mapping builds a customer query from `last_synced_at` through the due time, ordered by `id`.
The lower bound is inclusive and the upper bound is exclusive.
An absent checkpoint starts at `1970-01-01T00:00:00Z`.
Timestamp conversion normalizes the supplied timestamps to UTC, at second precision.
Use a NetSuite account time zone that matches these query bounds.

The page node carries the due time through every page, including an empty result.
The row mapping makes one `rest_consumer_fixture.item.record` command with each row's `id` as its key and `name` as its name.
The command result retains the due time as `request_id`.
The finish mapping refuses an error result before the `setting.write` node can run.
Only a successful batch advances `last_synced_at` to the due time.
The fixture command inserts missing keys. It does not update existing fixture rows.

The push registers on `rest_consumer_fixture.item` inserts.
It maps `new.key` to the external ID and `new.name` to `entityId`.
It sends a fixed PUT to `/services/rest/record/v1/customer/eid:<encoded-key>`.
A retry and a changed name both target the same path for that consumer key.
This gives one NetSuite record per consumer record across retries.
The generic HTTP delivery key is not the basis for this guarantee.
The pull and push use the existing consumer schema, which is also the schema that `rest_connector` registers.

The only setting key is `last_synced_at`, a JSON string that contains an RFC 3339 timestamp.
There are no account ID, realm, mapping, or enable settings.

## Connection

The org connection definition carries the account host.
The OAuth1 credential carries the account realm and signing keys.
The host signs requests. Guests receive neither endpoint authority nor credentials.
The connection requirement type is `http`. OAuth1 is its credential shape.

Set `WAMN_CONTROL_DATABASE_URL` to the control database URL.
Write this example definition to `netsuite-definition.json`:

```json
{"primary-authority":"https://1234567-sb1.suitetalk.api.netsuite.com","tls-verification":"verify-authority"}
```

Declare the org connection:

```sh
wamn-ctl connection declare --org acme --name netsuite \
  --requirement-type http --definition netsuite-definition.json
```

Use the actual account host in the definition and the actual account realm in the credential.
For example, a sandbox host uses `1234567-sb1`, while its realm is `1234567_SB1`.
The credential value has this JSON shape:

```json
{"oauth1":{"consumer-key":"<consumer key>","consumer-secret":"<consumer secret>","token":"<token>","token-secret":"<token secret>","realm":"1234567_SB1"}}
```

Place that JSON as a string in the mounted host credential file:

```json
{
  "projects": {},
  "orgs": {
    "acme": {
      "netsuite": "{\"oauth1\":{\"consumer-key\":\"<consumer key>\",\"consumer-secret\":\"<consumer secret>\",\"token\":\"<token>\",\"token-secret\":\"<token secret>\",\"realm\":\"1234567_SB1\"}}"
    }
  }
}
```

Bind each HTTP component's `upstream` requirement with `connections = { upstream = { org = "netsuite" } }` in the environment document.
Apply creates the application principal and grants its declared consumer dependency.
No manual consumer grant is needed.

Oracle publishes separate [SuiteTalk and RESTlet authorities](https://docs.oracle.com/en/cloud/saas/netsuite/ns-online-help/chapter_157011836591.html).
For a live RESTlet deployment, bind `http-request` to `<account>.restlets.api.netsuite.com` through its org definition.
That binding also affects record operations in the same deployment.
Use separate deployment bindings when both authorities are needed.
The local fixture serves all paths under one authority.
The guest cannot choose another host through a path.

## Build and tests

Build the base packages before this package:

```sh
TMPDIR=/var/tmp RUSTC_WRAPPER='' target/debug/wamn build \
  apps/platform/wamn_integration apps/rest_consumer_fixture apps/netsuite
```

Use the repository's native target directory for the mapping tests:

```sh
TMPDIR=/var/tmp RUSTC_WRAPPER='' CARGO_TARGET_DIR="$PWD/target" \
  cargo test --locked --offline --manifest-path apps/Cargo.toml \
  -p netsuite --target x86_64-unknown-linux-gnu --lib
```

Run publication refusal tests after the package build:

```sh
TMPDIR=/var/tmp RUSTC_WRAPPER='' cargo test --locked --offline \
  -p wamn-control --test netsuite_publication
```
