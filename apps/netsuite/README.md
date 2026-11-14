# NetSuite

This application composes `wamn_integration` and owns no domain tables.
The wirings implement every NetSuite request with JSONata and the platform HTTP nodes.
The Rust component forwards `setting.read` to the composed base.
Its private `sync.finish` command writes `last_synced_at` through the base inside the transaction that the host opens.
It contains no NetSuite request logic.
The package pins both base components by version and digest.

## Operations

Workflow examples live in `tests/workflows`.
Copy and edit these examples in the environment configuration, then author them with `workflow author`.
The package build does not publish workflows.

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
Its output holds `items`, `carry`, and the bound connection’s `external_system_id`.
The consumer mapping ignores any workflow parameter that names another source.
Queries must order by the unique `id` key.
The standalone query mapping refuses text without `ORDER BY id`.
The caller must keep that order as the query's actual final ordering clause.

## Pull and push

The pull is an environment workflow, not part of the release: an environment admin authors `netsuite_pull` with a minute schedule and enables it ([cutover](../../docs/operations/queued-automation.md#cutover-the-netsuite-pull)).
Each due moment runs `netsuite_pull` with its due time, as the `workflow` user with the role that holds the consumer command.
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
The account host and authentication belong to the connection.

## Connection

The `upstream` requirement declares `wamn:connection/http@0.1.0` with `mode = "write"`.
The connection uses `oauth2_client_credentials_jwt`, inherited from its `http` integration.
The host signs a JWT client assertion, a signed request for a token.
Guests receive neither the private key nor the token.

NetSuite refuses RS256.
Oracle permits PS256, PS384, PS512, ES256, ES384, and ES512.
The generic auth type supports PS256, ES256, and RS256, so select PS256 or ES256 for NetSuite.
NetSuite wants comma-separated scopes, so set `scope_separator` to `","`.
Oracle defines the algorithms, scopes, and JWT claims in [The Request Token Structure](https://docs.oracle.com/en/cloud/saas/netsuite/ns-online-help/section_162790605110.html).
The assertion uses the token endpoint as `aud` and the M2M certificate ID as `kid`.
The host sets `exp` to 300 seconds after `iat`, below Oracle's limit of 60 minutes.
It also sends a random `jti`, an identifier for each assertion.

Use these steps from the repository root.
The control host must already serve the organization, project, and environment.
See [integration administration](../../docs/operations/deployment.md#project-integrations-and-environment-connections) for its authority requirements.

1. Store the owner-supplied account details in `/private/netsuite/credentials.json`:

   ```json
   {
     "account_id": "1234567-sb1",
     "client_id": "<NetSuite integration client ID>",
     "certificate_id": "<NetSuite M2M certificate ID>",
     "algorithm": "PS256",
     "private_key_file": "/private/netsuite/key.pem"
   }
   ```

   `account_id` is the account hostname label, without a scheme or domain.
   `private_key_file` names the PEM private key for the registered certificate.
   Relative paths resolve from the current directory.
   Restrict access to both files:

   ```sh
   chmod 600 /private/netsuite/credentials.json /private/netsuite/key.pem
   ```

2. Set the control URL, session token, and target project and environment:

   ```sh
   export WAMN_CONTROL_URL='https://<organization-control-host>'
   export WAMN_TOKEN='<project-administrator-session-token>'
   NETSUITE_PROJECT=wms
   NETSUITE_ENV=dev
   NETSUITE_CREDENTIALS=/private/netsuite/credentials.json
   umask 077
   work=$(mktemp -d /var/tmp/wamn-netsuite-setup.XXXXXX)
   ```

3. Create or update the NetSuite integration with the `http` type:

   ```sh
   jq -n --arg project "$NETSUITE_PROJECT" \
     '{project:$project,name:"netsuite",integration_type:"http",auth_type:"oauth2_client_credentials_jwt"}' \
     > "$work/integration.json"
   wamn-ctl integration write --file "$work/integration.json"
   ```

4. Create the connection request with the account host and token endpoint:

   ```sh
   jq --arg project "$NETSUITE_PROJECT" --arg env "$NETSUITE_ENV" '
     ("https://" + .account_id + ".suitetalk.api.netsuite.com") as $origin |
     {project:$project,env:$env,name:"netsuite",parameters:{
       "primary-authority":$origin,"tls-verification":"verify-authority",
       auth:{client_id:.client_id,token_url:($origin + "/services/rest/auth/oauth2/v1/token"),
         scopes:["rest_webservices"],key_id:.certificate_id,algorithm:.algorithm,scope_separator:","}}}
   ' "$NETSUITE_CREDENTIALS" > "$work/connection.json"
   ```

   `token_url` is `https://<accountID>.suitetalk.api.netsuite.com/services/rest/auth/oauth2/v1/token`.
   `key_id` is the certificate ID from the NetSuite M2M setup.
   If the deployment uses RESTlets, add `"restlets"` to `scopes`, for example `["rest_webservices","restlets"]`.
   Oracle also permits `suite_analytics`.

5. Read the private key into a separate credential file:

   ```sh
   key_file=$(jq -er '.private_key_file' "$NETSUITE_CREDENTIALS")
   jq -n --rawfile private_key "$key_file" '{private_key:$private_key}' > "$work/credential.json"
   wamn-ctl connection write --file "$work/connection.json" --credential-file "$work/credential.json"
   ```

   The credential shape is `{private_key: PEM}`.
   Do not print the private key or put it in the connection request.

6. For a later key replacement, recreate the credential file from step 5 and call:

   ```sh
   jq -n --arg project "$NETSUITE_PROJECT" --arg env "$NETSUITE_ENV" \
     '{project:$project,env:$env,name:"netsuite"}' > "$work/target.json"
   wamn-ctl connection set-credential --file "$work/target.json" --credential-file "$work/credential.json"
   ```

7. Remove the temporary requests and credential after setup:

   ```sh
   rm -rf -- "$work"
   ```

Bind each HTTP component's `upstream` requirement with `connections = { upstream = "netsuite" }` in the environment document.
Select the published version with `integrations = { netsuite = { version = 1 } }`.
The pull runs as the `workflow` user with its execution role, so grant that role the consumer command.

Oracle publishes separate [SuiteTalk and RESTlet authorities](https://docs.oracle.com/en/cloud/saas/netsuite/ns-online-help/chapter_157011836591.html).
RESTlets use `<account>.restlets.api.netsuite.com`.
RESTlet connections set `parameters.auth.token_authority` to `<account>.suitetalk.api.netsuite.com:443`.
This permits the HTTPS token endpoint on SuiteTalk while the connection serves RESTlets.
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

Run the publication test after the package build.
It makes sure of the pull checkpoint through the application role and the refusal of invalid ports and dependencies:

```sh
TMPDIR=/var/tmp RUSTC_WRAPPER='' cargo test --locked --offline \
  -p wamn-environment --test netsuite_publication
```

## Owner-run live calls

Run `tools/netsuite/live.sh` with the credentials JSON from step 1.
The script creates or updates the integration and connection through the same commands as the setup above.
It calls `record.get` and `suiteql.query` once each, then waits for their results.
It never prints the private key and is never a test target.
It uses only `rest_webservices` because these calls use SuiteTalk.

Publish all seven wirings with the package.
Enable the manual workflows `record_get` and `suiteql_query` in the target application.
Set `WAMN_CONTROL_URL` and `WAMN_TOKEN` as above.
Set `WAMN_PG_ADMIN_URL` to the deployed project admin database URL.
The script needs `jq`, `psql`, and `wamn-ctl` on `PATH`.
If the CLI is elsewhere, set `WAMN_CTL` to its path.
Put a query with a final `ORDER BY id` clause in `/private/netsuite/query.sql`.

```sh
tools/netsuite/live.sh /private/netsuite/credentials.json wms dev netsuite \
  'https://<application-host>' 42 /private/netsuite/query.sql
```

`wamn-ctl workflow start` starts a workflow through the host and prints its run id.
It takes `--host-url`, `--token` (or `WAMN_TOKEN`), `--workflow-id`, `--idempotency-key` and `--input`.
A repeated start with the same key and input answers the first run.
Read the result of a run from `wamn_run.runs` in the project database.

## Kind fixture

The ignored `environment_apply_kind` target first applies a release with `wamn_integration`, `rest_connector` and `rest_consumer_fixture`.
It then applies a second release that replaces `rest_connector` with `netsuite`.
The NetSuite release binds one environment connection to the HTTP fixture, which acts as the SuiteTalk authority.
The fixture verifies an ES256 client assertion before it returns a bearer token.
The assertion carries the certificate ID, token audience, comma-separated scopes, and a five-minute lifetime.
The case does not call a RESTlet. The fixture unit tests cover the RESTlet echo route.

The test needs the platform images and stamped chart described in `environment_apply_kind.rs`.
Build `edge_device`, `wamn_integration`, `rest_consumer_fixture`, `rest_connector` and `netsuite`, plus the debug CLI and CDC reader, before the run.
The fixture image builds the named `http-fixture` binary in `tests/integration` from the repository root.

The case uses two SuiteQL pages, with a limit of two and `hasMore: false` on the second page.
It seeds four consumer rows and advances the private, inactive CDC slot past those seed writes before starting the reader.
The consumer command result proves that the pull delivered all four keys.
A later insert triggers one push, and a replay of its input sends the same external-ID path again.
The case reads HTTP evidence from `/recorded` and application state from PostgreSQL.
It authors and enables the pull as a minutely schedule workflow after seeding, and disables it after the first pull, so a second pull cannot race the assertions.
The lead runs the ignored kind target once at epic close.
