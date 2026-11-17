# Workflow operations

Workflow commands connect to the application host and require its `admin` role.
Set `WAMN_TOKEN` to the administrator's session token. Pass the application URL in `--host-url`.

## Author and enable a workflow

Write a format 0.2 graph with one entry node. The entry chooses Request, Schedule or Data change.
A Request declares a path such as `/orders/create`. Calls use POST with a JSON body at `/api/orders/flow/orders/create`.
Give each end of a Request path a Response node.
A Schedule declares a five-field UTC cron. A Data change entry names the source package, entity and row operations.
Specify `--execution-role` for a Schedule or Data change workflow. A Request with no execution role runs as its caller.

```bash
wamn workflow author --host-url "$APPLICATION_URL" --document orders-flow.json
wamn workflow enable --host-url "$APPLICATION_URL" --workflow-id orders --version 1
wamn workflow list --host-url "$APPLICATION_URL"
```

The host checks each call against the sibling name and export key in the live workload.
For a Request, grant `wamn-workflow:<application>/<workflow_id>` to each role that can call it.
The host returns the Response status and body. It waits up to 30 seconds.
Use `workflow disable` with the same flags as `workflow enable` to stop new admissions.
For Data change, enable creates the workflow's pull consumer. Disable removes it after the database commits.
If a replacement needs a different consumer filter or start sequence, disable the current version before enabling the replacement.

## Run now

Use `--run-as admin` to run as the caller. If the workflow declares an execution role, use `--run-as workflow` to select that role.

```bash
wamn workflow run --host-url "$APPLICATION_URL" --workflow-id orders \
  --run-as admin --idempotency-key batch-42 --input orders-input.json
```

The command returns the run ID after admission. Read the result in `wamn_run.runs`.
Repeating the same workflow, key and input returns the first run ID. Reusing the key with another input refuses.
For a Request graph, the result records the Response.
The host reads the executing role's grants before each node.

## Cutover the NetSuite pull

Deploy the `netsuite` application with `wamn_integration` and the consumer package.
Create the `netsuite-pull` role on the application host. Grant it the consumer's record operation.
Copy [the pull example](../../apps/netsuite/tests/workflows/netsuite_pull.json) to `netsuite-pull.json`.
Set its call parameters for the environment.

```bash
wamn workflow author --host-url "$APPLICATION_URL" --document netsuite-pull.json \
  --execution-role netsuite-pull
wamn workflow enable --host-url "$APPLICATION_URL" --workflow-id netsuite_pull --version 1
```

The example runs each UTC minute as the `workflow` user.
Its checkpoint is in `wamn_integration.setting`.
