# Queued automation

`wamn-ctl workflow start` starts one run of an environment workflow through the host route `workflow.start`.
The route is an operation of the application's ACL: `admin` holds it, and another role holds it through a stored permission row of `wamn-control:workflow/start`.
The other `workflow` commands call the workflow contract in `wamn-workflow`, and they use project-admin database authority.
The host's queue worker uses its existing database roles to claim and execute the run.

Author and enable the workflow first with `wamn-ctl workflow author` and `wamn-ctl workflow enable`.
Every `workflow` command except `start` needs `--tenant`, `--environment`, and the project-admin database URL.
Pass the URL with `--admin-database-url` or `WAMN_PG_ADMIN_URL`.
`--schema` selects the run-plane schema and defaults to `wamn_run`.
`author` and `enable` also read the platform inputs of `wamn-ctl env`.
`author` requires these flags:

- `--coordinate <org>/<project>/<env>`
- `--control-database-url`
- `--application`
- `--document`
- `--execution-type role|invoker`
- `--principal-id`

`author` also requires `--execution-role` with `--execution-type role`, and refuses it with `invoker`.
`enable` requires `--coordinate`, `--application`, `--workflow-id`, `--version`, and `--principal-id`.
`disable` takes the same flags as `enable`, except `--principal-id`.
A workflow of type `role` runs as the environment's `workflow` user with its execution role, so grant that role to the `workflow` user.
A workflow of type `invoker` runs as the caller with its active role.
A caller with `admin` acts as `admin`, and a caller with exactly one role acts with that role.
A caller with no role or more than one role is refused with `active_role_required`.

`wamn-ctl workflow author --schedule '<cron>'` authors a schedule workflow: a five-field cron, evaluated in UTC.
An invalid cron is refused at author.
`workflow.start` does not start a schedule workflow, and it refuses `workflow_not_enabled`.
A schedule run executes as the `workflow` user: with its execution role for type `role`, and with `admin` for type `invoker`.
So to enable an `invoker` schedule, the enabler must hold `admin`, and the `workflow` user must hold `admin`.

Configure the host with the selected release and both executor-class and callable-HTTP database credentials.
The executor credential reads the enabled workflow and admits the run.
The callable-HTTP credential reads the principal's current roles and permissions.

Set `WAMN_TOKEN` to a session token or a PAT of the caller.
Then submit an input file:

```bash
wamn-ctl workflow start \
  --host-url "https://$APPLICATION_HOST" --application orders --workflow-id nightly \
  --idempotency-key batch-42 --input /tmp/orders-input.json
```

The command prints the stored run id.
`--application` names an application that the environment document declares under `applications`.
The host refuses another name with `application_not_declared`.
The host admits the enabled version against the release it serves.
It gates the graph against that release and refuses `workflow_not_served` when the graph does not gate or its hash differs from the stored hash.
A workflow with no enabled version refuses `workflow_not_enabled`.
The run records the release, the graph hash, the component digest of each node, the principal and role it executes as, and the caller as its invoker.
An identical retry by the same caller returns that id and creates no second queue row.
The same key with another input refuses `idempotency_conflict`.
The run and queue row commit in one transaction.

A workflow run has no waiting HTTP caller.
Use an emit terminal, or let the graph finish without a terminal.
Before each node, the host reads the principal's current grants for the run's role, and applies the normal operation checks.
A role revoked during a run refuses the next node.
Operations that require a fresh PAT refuse a queued run.
The executing principal owns application writes, while the `wamn:executor` credential class owns queue maintenance.

## Park, release, and list runs

Each command needs `--tenant` and `--environment`. `park` and `release` also need `--run-id`.
`runs` takes an optional `--limit`, which defaults to 50.

```bash
wamn-ctl workflow park --tenant acme --environment dev --run-id "$RUN_ID"
wamn-ctl workflow release --tenant acme --environment dev --run-id "$RUN_ID"
wamn-ctl workflow runs --tenant acme --environment dev --limit 20
```

A park holds a queued run that no worker holds, so no worker claims it.
The run keeps the status `dispatched`, and a park of a parked run changes nothing.
A running or finished run refuses the park.
A release returns a parked run to the queue, and a run that is not parked refuses it.
`runs` prints one JSON object per run, newest first, with its status and its `queued` and `parked` flags.

## Declared workflows

Pass the database URL as above, then select the installed release with the required `--release-digest`:

```bash
wamn-ctl workflow declared --tenant acme --environment dev --release-digest "$RELEASE_DIGEST"
```

The command prints each declared workflow with its event registration.
`workflow runs` prints runs, and `workflow list` prints the workflow versions authored in the environment.
A package declares no schedule.
Author a schedule workflow in the environment with `wamn-ctl workflow author --schedule '<cron>'`, then enable it.

Every application host runs one schedule task.
At each minute it reads, in one query, the enabled schedule workflows of the applications its environment declares, and admits a run for each one whose cron names that minute.
`wamn-ctl workflow author` refuses an application that the environment does not declare.
The task uses `WAMN_EXECUTOR_PLATFORM_PG_URL`.
An enable or a disable takes effect at the next minute.
See [workflows](../architecture/execution.md#workflows) for timing and retry behavior.

## Cutover: the NetSuite pull

A release no longer carries the NetSuite pull's minute schedule.
An environment that ran the pull from its release, such as wamn-dev, stops pulling when it applies such a release.
That apply also declares the environment's applications, which `env apply` now requires.
Add one application `netsuite` to the environment document of the wamn-dev NetSuite environment, and list every package of its release in it:

```python
    applications = {
        netsuite = {packages = ["netsuite", "wamn_integration", "<consumer package>"]}
    }
```

`env apply` refuses a release package that no application lists, a package that two applications list, and a listed package that the release does not carry.
Do these steps once, after that apply, as an environment admin.
Set `WAMN_SYSTEM_ADMIN_URL`, `WAMN_PG_ADMIN_URL`, the coordinate `<org>/<project>/<env>`, its tenant, and your `platform."user"` id as `$ADMIN_ID`.

1. Create the role the pull executes with. It holds exactly the consumer command that the pull's `record` node calls:

   ```bash
   wamn-ctl create-role --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$WAMN_PG_ADMIN_URL" \
     --org <org> --project <project> --env <env> --tenant <tenant> --role netsuite-pull
   wamn-ctl grant-permission --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$WAMN_PG_ADMIN_URL" \
     --org <org> --project <project> --env <env> --tenant <tenant> --role netsuite-pull \
     --operation <consumer package>:<interface>/<operation>
   ```

2. Grant the role to the `workflow` user, whose email is `workflow@<platform domain>`, with `wamn-ctl grant-role --role netsuite-pull`.
   The enabler must also hold the role, or hold `admin`.
3. Author the released wiring `apps/netsuite/publication/wirings/netsuite_pull.json` as a schedule workflow of the application `netsuite`:

   ```bash
   wamn-ctl workflow author --tenant <tenant> --environment <env> --coordinate <org>/<project>/<env> \
     --control-database-url "$WAMN_SYSTEM_ADMIN_URL" --application netsuite \
     --document apps/netsuite/publication/wirings/netsuite_pull.json \
     --execution-type role --execution-role netsuite-pull --schedule '* * * * *' --principal-id "$ADMIN_ID"
   ```

   The author needs a green gate report under the document's hash, as for any workflow.
4. Enable it:

   ```bash
   wamn-ctl workflow enable --tenant <tenant> --environment <env> --coordinate <org>/<project>/<env> \
     --application netsuite --workflow-id netsuite_pull --version 1 --principal-id "$ADMIN_ID"
   ```

The next minute admits the first pull.
Its run key is `schedule:netsuite/netsuite_pull:<due>`, and the run executes as the `workflow` user with `netsuite-pull`.
`wamn-ctl workflow list` shows the enabled version, and `wamn-ctl workflow disable` stops the pull.
The pull keeps its checkpoint in `wamn_integration.setting`, so the first pull after cutover continues from the last pull of the release.
