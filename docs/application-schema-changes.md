# Application schema changes

A schema is the structure of the database. A migration changes that structure or its data. A breaking schema change prevents earlier application code from using the database as it expects.

## Plan the change

The deploy worker applies SQL before it upgrades the application. Earlier code can still serve during that upgrade. You decide which earlier versions must work with the migrated schema.

If earlier code must keep serving, make the first migration compatible with both versions. For example, add a column, deploy code that uses it, and remove the old column in a later release. Test earlier code against the migrated schema before you deploy a change that relies on this compatibility.

## Mark the package version

Use a new `package.version` to mark a breaking schema change. If you use semantic versions, increase the major version, for example from `1.4.2` to `2.0.0`. WAMN treats the version as tag text. The package digest identifies the exact published content.

Keep every applied migration file with its original name and contents. Give each new migration a higher number. Publishing to a new tag starts a new publication history for that tag. The database still checks its applied migration history when you deploy.

## Recover after a failed deploy

If a new release fails to serve, WAMN attempts to restore the previous release on the migrated schema. If Helm returns success but the new workload never becomes Ready, WAMN rolls back and checks that the restored workload serves. Committed migrations stay installed. If the restored code cannot use that schema, publish and deploy a corrective migration with a higher number to move the schema forward.

To pick earlier code explicitly, use `wamn rollback APP --revision N --env env.yaml`. Without `--revision`, WAMN selects the newest earlier deploy. Choose a revision that works with the schema already installed.
