# Queued automation

`wamn workflow start` starts one run of an environment workflow through the host route `workflow.start`.
The route is an operation of the application's ACL: `admin` holds it, and another role holds it through a stored permission row of `wamn-control:workflow/start`.
The other `workflow` commands call org control routes. The service checks current project administration authority.
The host's queue worker uses its existing database roles to lease and execute the run.

Author and enable the workflow first with `wamn workflow author` and `wamn workflow enable`.
Author, enable and disable use `--coordinate <org>/<env>/<project>` and control credentials. List, park, release and runs use `--project` and `--environment`.
`author`, `enable`, and `disable` take the organization and project from `--coordinate`.
`list`, `park`, `release`, and `runs` require `--project`.
Workflow administration uses `WAMN_CONTROL_URL` and `WAMN_TOKEN`.
The server selects the database, serving release and authenticated author.
`author` requires these flags:

- `--coordinate <org>/<env>/<project>`
- `--application`
- `--document <compiled-publication-wiring>`
- `--execution-type role|invoker`

`author` also requires `--execution-role` with `--execution-type role`, and refuses it with `invoker`.
`enable` requires `--coordinate`, `--application`, `--workflow-id`, and `--version`.
`disable` takes the same flags as `enable`.
A workflow of type `role` runs as the environment's `workflow` user with its execution role, so grant that role to the `workflow` user.
A workflow of type `invoker` runs as the caller with its active role.
A caller with `admin` acts as `admin`, and a caller with exactly one role acts with that role.
A caller with no role or more than one role is refused with `active_role_required`.

`wamn workflow author --schedule '<cron>'` authors a schedule workflow: a five-field cron, evaluated in UTC.
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
wamn workflow start \
  --host-url "https://$APPLICATION_HOST" --application orders --workflow-id nightly \
  --idempotency-key batch-42 --input /tmp/orders-input.json
```

The command prints the stored run id.
`--application` names an application that the environment document declares under `applications`.
The host refuses another name with `application_not_declared`.
The host admits the enabled version against the release it serves.
It evaluates the graph conditions against that release and refuses `workflow_not_served` when the graph fails its conditions or its hash differs from the stored hash.
A workflow with no enabled version refuses `workflow_not_enabled`.
The run records the release, the graph hash, the component digest of each node, the principal and role it executes as, and the caller as its invoker.
An identical retry by the same caller returns that id and creates no second queue row.
The same key with another input refuses `idempotency_conflict`.
The run and queue row commit in one transaction.

A workflow run has no waiting HTTP caller.
Use an emit terminal, or let the graph finish without a terminal.
Before each node, the host reads the principal's current grants for the run's role, and applies the normal operation tests.
A role revoked during a run refuses the next node.
Operations that require a fresh PAT refuse a queued run.
The executing principal owns application writes, while the `wamn:executor` credential class owns queue maintenance.

## Park, release, and list runs

Each command needs `--project` and `--environment`. `park` and `release` also need `--run-id`.
`runs` takes an optional `--limit`, which defaults to 50.

```bash
wamn workflow park --project orders --environment dev --run-id "$RUN_ID"
wamn workflow release --project orders --environment dev --run-id "$RUN_ID"
wamn workflow runs --project orders --environment dev --limit 20
```

A park holds a queued run that no worker holds, so no worker leases it.
The run keeps the status `dispatched`, and a park of a parked run changes nothing.
A running or finished run refuses the park.
A release returns a parked run to the queue, and a run that is not parked refuses it.
`runs` prints one JSON object per run, newest first, with its status and its `queued` and `parked` flags.

## Authored workflows

`wamn workflow list --project orders --environment dev` lists authored workflow versions.
`workflow runs` lists their runs. A package declares no schedule.
Author a scheduled workflow with `wamn workflow author --coordinate <org>/<env>/<project> --schedule '<cron>'`, then enable it.

Every application host runs one schedule task.
At each minute it reads, in one query, the enabled schedule workflows of the applications its environment declares, and admits a run for each one whose cron names that minute.
`wamn workflow author` refuses an application that the environment does not declare.
The task uses `WAMN_EXECUTOR_PLATFORM_PG_URL`.
An enable or a disable takes effect at the next minute.
See [workflows](../architecture/execution.md#workflows) for timing and retry behavior.

## Cutover: the NetSuite pull

A release no longer carries the NetSuite pull's minute schedule.
An environment that ran the pull from its release, such as wamn-dev, stops pulling when it applies such a release.
That apply also declares the environment's applications, which `environment::apply` now requires.
Add one application `netsuite` to the environment document of the wamn-dev NetSuite environment, and list every package of its release in it:

```python
    applications = {
        netsuite = {packages = ["netsuite", "wamn_integration", "<consumer package>"]}
    }
```

`environment::apply` refuses a release package that no application lists, a package that two applications list, and a listed package that the release does not carry.
Do these steps once, after that apply, as an environment admin.
Set `WAMN_CONTROL_URL` to the application host and `WAMN_TOKEN` to your session token.
Set the coordinate `<org>/<project>/<env>`, its composed environment key, and your application user ID as `$ADMIN_ID`.

1. Create the role the pull executes with. It holds exactly the consumer command that the pull's `record` node calls:

   ```bash
   wamn create-role --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" --role netsuite-pull
   wamn grant-permission --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" --role netsuite-pull \
     --operation <consumer package>:<interface>/<operation>
   ```

2. Grant the role to the `workflow` user, whose email is `workflow@<platform domain>`, with `wamn grant-role --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" --user-id <workflow UUID> --role netsuite-pull`.
   The enabler must also hold the role, or hold `admin`.
3. Build the NetSuite package with `wamn build` first. Copy the example in `apps/netsuite/tests/workflows/netsuite_pull.json` to `netsuite-pull.json` in the environment configuration. Edit its parameters, then author it as a schedule workflow of the application `netsuite`:

   ```bash
   wamn workflow author --coordinate <org>/<env>/<project> \
     --application netsuite \
     --document netsuite-pull.json \
     --execution-type role --execution-role netsuite-pull --schedule '* * * * *'
   ```

   Authorship validates the graph against the serving release.
4. Enable it:

   ```bash
   wamn workflow enable --coordinate <org>/<env>/<project> \
     --application netsuite --workflow-id netsuite_pull --version 1
   ```

The next minute admits the first pull.
Its run key is `schedule:netsuite/netsuite_pull:<due>`, and the run executes as the `workflow` user with `netsuite-pull`.
`wamn workflow list` shows the enabled version, and `wamn workflow disable` stops the pull.
The pull keeps its checkpoint in `wamn_integration.setting`, so the first pull after cutover continues from the last pull of the release.
