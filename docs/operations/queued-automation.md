# Queued automation

`wamn-ctl enqueue-run` queues one released wiring under a service principal.
The command uses project-admin database authority.
The executor uses its existing database roles to claim and execute the run.

First, provision the service identity and reconcile its tenant user row.
Assign its application roles in `app_system.user_roles` through the project administrator.
Those roles grant operations through `app_system.permissions`.
The service row must have type `service` and status `active`.

Reconcile the run schema before admission.
Configure the executor with the selected release and both executor and callable-HTTP database credentials.
The callable-HTTP credential reads the service identity and its current permissions.

Set `WAMN_PG_ADMIN_URL` to the project-admin connection URL.
Then submit an input file:

```bash
wamn-ctl enqueue-run \
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
The executor reads current service permissions before delivery and applies the normal operation checks.
Nested calls retain the service principal.
Operations that require a fresh PAT refuse queued automation.
The service principal owns application writes, while `wamn:executor` owns queue maintenance.
