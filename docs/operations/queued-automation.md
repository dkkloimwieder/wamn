# Queued automation

`wamn-ctl workflow start` queues one released wiring under a service principal.
The `workflow` commands call the workflow contract in `wamn-workflow`, and they use project-admin database authority.
The host's queue worker uses its existing database roles to claim and execute the run.

First, provision the service identity and reconcile its tenant user row.
Assign its application roles in `app_system.user_roles` through the project administrator.
Those roles grant operations through `app_system.permissions`.
The service row must have type `service` and status `active`.

Reconcile the run schema before admission.
Configure the host with the selected release and both executor-class and callable-HTTP database credentials.
The callable-HTTP credential reads the service identity and its current permissions.

Set `WAMN_PG_ADMIN_URL` to the project-admin connection URL.
Then submit an input file:

```bash
wamn-ctl workflow start \
  --tenant acme --environment dev --package-id orders \
  --effective-release-id 1 --wiring-id process-orders --wiring-version 1 \
  --service-principal-id "$SERVICE_PRINCIPAL_ID" \
  --idempotency-key batch-42 --input /tmp/orders-input.json
```

The command prints the stored run id.
An identical retry returns that id and creates no second queue row.
A changed request with the same tenant and key refuses.
The run and queue row commit in one transaction.
The run records the released wiring hash, service identity, and environment durability policy.

Automation has no waiting HTTP caller.
Use an emit terminal, or let the graph finish without a terminal.
The host reads current service permissions before delivery and applies the normal operation checks.
Nested calls retain the service principal.
Operations that require a fresh PAT refuse queued automation.
The service principal owns application writes, while the `wamn:executor` credential class owns queue maintenance.

## Park, release, and list

Use the same `--tenant` and `--environment` flags for each command.

```bash
wamn-ctl workflow park --tenant acme --environment dev --run-id "$RUN_ID"
wamn-ctl workflow release --tenant acme --environment dev --run-id "$RUN_ID"
wamn-ctl workflow list --tenant acme --environment dev --limit 20
```

A park holds a queued run that no worker holds, so no worker claims it.
The run keeps the status `dispatched`, and a park of a parked run changes nothing.
A running or finished run refuses the park.
A release returns a parked run to the queue, and a run that is not parked refuses it.
The list prints one JSON object per run, newest first, with its status and its `queued` and `parked` flags.
