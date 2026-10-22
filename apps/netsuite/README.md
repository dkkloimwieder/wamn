# NetSuite

This application composes `wamn_integration` and owns no domain tables.
The wirings implement every NetSuite request with JSONata and the platform HTTP nodes.
The Rust component forwards `setting.read` to the composed base.
Its private `sync.finish` command writes `last_synced_at` through the base inside the transaction that the host opens.
It contains no NetSuite request logic.
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
The finish mapping refuses an error result before the `sync.finish` command can run.
It passes the due time to that command.
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

Run the publication and live checkpoint tests after the package build:

```sh
TMPDIR=/var/tmp RUSTC_WRAPPER='' cargo test --locked --offline \
  -p wamn-control --test netsuite_publication
```

## Owner-run live calls

`tools/netsuite/live.sh` calls the deployed `record_get` and `suiteql_query` wirings once each through `wamn-ctl workflow start`.
It prints each completed run and its result from `wamn_run.runs`.
SuiteQL can send several HTTP pages within that one operation.
The script never runs from a test target.

Create a private JSON file with the real account values:

```json
{
  "account_host": "1234567-sb1.suitetalk.api.netsuite.com",
  "realm": "1234567_SB1",
  "consumer_key": "<consumer key>",
  "consumer_secret": "<consumer secret>",
  "token_id": "<token id>",
  "token_secret": "<token secret>"
}
```

Prepare files on the owner machine:

```sh
chmod 600 /private/netsuite.json
tools/netsuite/live.sh prepare /private/netsuite.json acme /private/netsuite-host
```

Declare `netsuite` with the generated `definition.json` through the connection command above.
Mount the generated `credentials.json` as the host credential file, or merge its org entry into the existing file.
Set the deployed host's `WAMN_CREDENTIALS_FILE` to that mount.
Bind `upstream` to `{ org = "netsuite" }`, then apply the environment and restart the host with that file.
Publish all seven wirings, including `record_get` and `suiteql_query`, with the package.
The selected `record_get` wiring supplies the record type, which defaults to `customer`.

The owner machine needs `jq`, `psql`, and the built `wamn-ctl` binary.
Set `WAMN_PG_ADMIN_URL` to the deployed project database and `WAMN_CONTROL_DATABASE_URL` to the control database.
Set local `WAMN_CREDENTIALS_FILE` to the same source file that the host mounts.
The script compares that source with the supplied keys and compares the org connection with `account_host`.
The owner must make sure that the running host uses that source and connection binding.
The script does not deploy or rotate credentials.

Save an ordered SuiteQL query in `/private/customer.sql`, then run:

```sh
export WAMN_PG_ADMIN_URL='<project admin PostgreSQL URL>'
export WAMN_CONTROL_DATABASE_URL='<control PostgreSQL URL>'
export WAMN_CREDENTIALS_FILE=/private/netsuite-host/credentials.json
printf '%s\n' 'SELECT id, entityid FROM customer ORDER BY id' > /private/customer.sql
tools/netsuite/live.sh run /private/netsuite.json acme acme--billing--dev dev \
  'sha256:<installed release digest>' '<NetSuite application principal UUID>' \
  '42' /private/customer.sql
```

Use the principal that apply creates for `netsuite`.
A timeout prints the run ID because the deployed run can continue after the script exits.
Inspect that run before another invocation.

## Kind fixture

The ignored `environment_apply_kind` target runs the Epic 4 release, then a separate NetSuite release in the same private cluster.
The NetSuite release contains `wamn_integration`, `rest_consumer_fixture`, and `netsuite`.
It binds one org connection to the HTTP fixture, which acts as the SuiteTalk authority.
The case does not call a RESTlet. The fixture unit tests cover the RESTlet echo route.

The lead supplies the platform images and stamped chart described in `environment_apply_kind.rs`.
Build the three packages above and `rest_connector`, plus the debug CLI and CDC reader, before the closeout run.
Export `WAMN_HTTP_FIXTURE_CONSUMER_KEY`, `WAMN_HTTP_FIXTURE_CONSUMER_SECRET`, `WAMN_HTTP_FIXTURE_TOKEN`, `WAMN_HTTP_FIXTURE_TOKEN_SECRET`, and `WAMN_HTTP_FIXTURE_REALM`.
Use disposable fixture values, not real account credentials.
The test passes these values independently to the host credential mount and the fixture process.
The fixture image builds from `tests/support/http-fixture` under `crates/control/lib`.

The case uses two full SuiteQL pages, with a limit of two and `hasMore: false` on the second page.
It seeds four consumer rows and advances the private, inactive CDC slot past those seed writes before starting the reader.
The consumer command result proves that the pull delivered all four keys.
A later insert triggers one push, and a replay of its input sends the same external-ID path again.
The case reads HTTP evidence from `/recorded` and application state from PostgreSQL.
It uses a daily tick five minutes ahead of publication, so a second pull cannot race the assertions.
The lead runs the ignored kind target once at epic close.
