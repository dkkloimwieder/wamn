# 82 wamn-control-registry, hierarchy order

Scope: the follow-up carried from tab 65 finding 4: the code orders org, project, environment; the ruled order is org, environment, project, application. Read at `epic/dep7` (b94833fd): `crates/control/registry/src/types.rs`, the `registry` and `identity` tables of `deploy/sql/system-schema.sql`, and name builders in `wamn-control-provision`. Tab 02 reviewed the registry crate itself. Nothing built or run.

## Earlier work checked first

- Tab 65 4 A (2026-11-12): org admin is admin in every environment; an environment admin is admin of every project and application in that environment and is not an org admin; a project admin is admin of the applications in that project; all derived when authorizing, no copied rows.
- Tab 62 3 B: the registry holds environment configuration. Tab 63 1 A: an application is its release.
- Tab 76 (follows): `HostRouteAuthority` has no environment admin.
- Greenfield: no migration of old rows.

## Files read

- `registry/src/types.rs`: `Triple`, `project_env_key`, `environment_nats_user`, `control_nats_user`.
- `system-schema.sql`: `registry.orgs`, `registry.env_policies`, `registry.projects`, `registry.project_envs`, `identity.org_roles`, `identity.project_roles`, `identity.project_env_memberships`.
- `provision/src/environment_state.rs::environment_resource_name`, `provision/src/name.rs::project_env_database_name`.

## Callers read

- Repository grep counts: `project_env_key` in 55 Rust files, `Triple` in 73, `project_envs` in 77 Rust and SQL files; `evt.{}.{}.{}` subject builders in 7.

## Facts

- There is no environment entity. An environment exists only as a `registry.project_envs` row keyed `(org, project, env)` under `registry.projects`; it carries `policy_name`, `status`, `disposable`, `route_host`, the database secret and the instance suffix.
- Environment policies are org-level rows (`registry.env_policies`, key `(org, name)`).
- Roles: `identity.org_roles` (org admin) and `identity.project_roles` (project admin); membership is `identity.project_env_memberships (principal, org, project, env)`. No environment role exists.
- Names follow org, project, environment: `env_key = <org>--<project>--<env>`, `Triple { org, project, env }` and its display `org/project/env`, event subjects `evt.<org>.<project>.<env>`, database names, and the hash input of `environment_resource_name` (`org/project/env`).

## Findings

**1. The data model has no environment above projects.**

Options:

- A: add `registry.environments (org, env)` holding what belongs to the whole environment (policy, status, disposable; inference which of today's columns move). Projects stay org-level definitions. `registry.environment_projects (org, env, project)` replaces `project_envs` and keeps the per-project items (database secret, instance suffix, route host). `identity.environment_roles (principal, org, env, role)` holds environment admins; memberships key `(org, env, project)`. Every name orders org, environment, project: the key `<org>--<env>--<project>`, the `Triple` fields, event subjects, database and resource names.
- B: keep the tables; add only `identity.environment_roles` keyed `(org, env)` with no parent row, and reorder keys and names.

Recommend A: an environment admin needs an environment to be admin of, and environment-wide settings get one owner.

## Follows from earlier rulings

- Tab 65 4 A: authorization derives admin by rule: org admin, then environment admin of the request's environment, then project admin of its project. `HostRouteAuthority` gains environment admin (tab 76).
- Tab 63 1 A: applications sit under projects as releases; no registry table for applications.

## Fine as is

- `control_nats_user` (per org) and `environment_nats_user(env_key)`: only the key inside changes.

## Tests deleted, changed or moved

- Finding 1 A: tests that build or parse `env_key`, `Triple`, `project_envs` rows and event subjects change with the order; authorization tests gain the environment admin cases.

## Not read

- `registry/src/resolve.rs`, `template.rs`, `validate.rs` bodies; the 55, 73 and 77 call sites one by one.

## Rulings

- 2026-11-12: finding 1 A (registry.environments as the parent; environment\_projects replaces project\_envs; identity.environment\_roles; every key, subject and name ordered org, environment, project).
