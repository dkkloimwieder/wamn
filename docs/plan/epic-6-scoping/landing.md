# Landing page, chooser and default application

Scope: bead E6.7 (`wamn-lb3lo.7`), T11 ruling 8 of 2026-11-27. The owner ruled three facts. The default application is a setting of the user, not of the env. After login at the org address, a user lands on the default application, or chooses when there are several. The chooser lists only applications the user may use. This document designs the rest. The code was read at `51bf22c7c`.

F means read in the code at that commit. I means inference or proposal. Each open point gives options and one recommendation.

## Current state

- Web serves the pinned Platform UI `index.html` at the root of the org host, and the pinned app `index.html` on each app host (F, `services/web/src/lib.rs:59`, `:110`, `:301`).
- On kind, no Platform UI is pinned. `deploy/charts/org/values.yaml:12` sets `platformUi: null`, and the platform build pins it only when a GCP web bucket is set (F, `xtask/src/platform.rs:63`). On kind, web has no object store, so a static page request gets 503 (F, `services/web/src/lib.rs:132`, `:453`). Kind tests reach routes and `config.json`, not pages.
- Platform UI shows the sign-in form at `/`. It posts to `/password/session`, then goes to `/urn:wamn:org:<org>` on the same host. The login has no return-to parameter (F, `apps/platform_ui/src/identity.tsx:85`, `:225`, `apps/platform_ui/src/shell.tsx:385`).
- No store has a default application field. `identity.principal` and `identity.org_membership` have none. Control owns `identity.org_membership`, keyed by `principal_id, org` (F, `deploy/sql/system-schema.sql:220`, `:286`, `services/control/src/control_route/users.rs:74`).
- `identity.has_project_access` decides whether a user can use a project in an env: an active org, project, project-env, principal and membership, plus `org-admin`, `project-admin` or an explicit project-env membership (F, `deploy/sql/system-schema.sql:1361`, `:1375`, `:1403`). Delivery of an app's index page has no user check (F, `services/web/src/lib.rs:388`).
- Control has list routes (`control/mine`, `project/list`, `env/list`, `member/list`, `user/list`) and no app list (F, `crates/routes/src/host_route.rs:363`, `:645`). `control/mine` is for administrators.
- The access token carries `iss`, `sub`, the org `aud`, `exp`, `iat`, `jti` and `client_id`, and no role, project or env (F, `crates/identity-protocol/src/records.rs:114`). The host derives the active role for the target project on each request: `admin`, or the only role held, or none (F, `crates/app/src/ingress.rs:167`, `crates/authentication/src/authorization.rs:20`).
- Web answers 404 for a host with no route (F, `services/web/src/lib.rs:98`).

## 1. Where the setting is stored

- A. Two nullable columns on `identity.org_membership`: `default_project` and `default_app`, with the project and app slug checks. One setting per user and org. Control owns the table, so identity does not change (I).
- B. A column on `identity.principal`. A principal can belong to several orgs, so the value needs the org too, and identity owns the table (I).
- C. A user setting in each project database (`platform.setting`, F, `deploy/sql/app-schema.sql:432`). The landing page needs the value before it knows the project (I).

Recommendation: A. The value names an application by project and app, not by env, because the setting is of the user. It has no foreign key, because an app can exist in one env and not in another. A value that names no application the user can use is ignored (I).

## 2. The list operation and its wire

- A. A new control route `GET /wamn_control/app/mine`. One SQL read joins `registry.env_app` with the active envs of the org, filtered by `identity.has_project_access` for the caller. It runs on the existing control pool and login (I).
- B. Extend `control/mine`. It changes an administrator contract (I).
- C. An identity route. Identity cannot read `registry.env_app` today (F, `crates/platform-install/src/identity_role.rs:13`).

Recommendation: A, with this response (I):

```json
{
  "default": { "project": "<project>", "app": "<app>" },
  "apps": [
    { "env": "<env>", "prod": true, "project": "<project>", "app": "<app>", "url": "https://<org>.<domain>/<project>/<app>/" }
  ]
}
```

- `default` is null when no setting exists. `url` comes from the product address functions of E6.3.
- The list covers every env of the org, so the page needs no env input. It keeps the entries whose host is its own host.
- An entry is eligible when the user has project access in that env and the env has an active app with a browser route (I). A check that the user holds an operation of the app needs the project databases, and the host already refuses operations the user does not hold (F, `crates/authentication/src/authorization/project_reads.rs:24`).
- A second route `PUT /wamn_control/app/default` sets or clears the caller's own setting, with `{ "project", "app" }` or null. Control refuses a value that is in no entry of the caller's list (I).
- Both routes are in `HostRouteSet::Control`. The OpenAPI document and the `wamn_control_org` SDK are generated again (F, `xtask/src/platform.rs:38`, `apps/platform_ui/openapi-ts.config.ts:5`).
- Each route runs one SQL statement and makes no network call inside the transaction (I).

## 3. What the landing page serves, and from where

- A. Platform UI at the host root, after sign-in. It calls `app/mine`, then opens the default, or the only entry, or shows the chooser. The chooser has a "Make default" action that calls `app/default` (I).
- B. A page that web renders in `edge-web`. Web then calls control for the list on each root request (I).
- C. An identity page. Identity has no page handler and no read of the app registry (F, `services/identity-service/src/password.rs:392`).

Recommendation: A. It adds no service and no image beyond the E6.8 builds of `control` and the browser bundles (I). On kind, no page is served (F, see current state), so the routes are tested by a journey, and the page by the Platform UI unit tests.

Org administrators also land on the landing page. The chooser shows a link to the org console at `/urn:wamn:org:<org>` when the user is an org administrator (I).

## 4. No application, one application, several

- No eligible entry on this host: the page says that no application is available, and offers sign-out. An org administrator also sees the console link (I).
- One eligible entry: the page opens it, with no chooser (I).
- Several entries and a default among them: the page opens the default.
- Several entries and no default among them: the page shows the chooser.

Option for one entry: show a chooser with one row. Recommendation: open it, because the ruling says that the user chooses only when there are several.

## 5. An org with no prod env, and env hosts

The org apex `<org>.<domain>` serves org control and identity at the root, with or without a prod env (T11, ruling 1). The control routes and the root page are on every env host of the org too, because session cookies are host-only (F, `services/web/src/browser_session.rs:217`).

- A. With no prod env, the apex page lists the user's apps on the env hosts, each as a link. The user signs in again on that host, because the cookie does not cross hosts (I).
- B. The apex page says that the org has no production environment, and lists nothing.
- C. The apex redirects to one env host.

Recommendation: A. The list of section 2 covers every env, so it needs no extra read. On an env host, the page uses only the entries of that host.

## 6. A session's active role across projects

- A. Keep the current rule: the session carries no role, and the host derives the active role for the target project on each request (F, `crates/app/src/ingress.rs:167`). One session covers every app on its host, in every project the user can use. Moving between apps needs no role change.
- B. An explicit role choice per project in the session. It adds a request input and a check against the roles held in the target project (I).

Recommendation: A. A user with several roles in one project and no `admin` role has no active role today, and operation checks use the combined grants (F, `crates/authentication/src/authorization.rs:20`). That stays.

## Changes if the recommendations are ruled

- Schema: `identity.org_membership.default_project` and `default_app`, nullable, in the bootstrap SQL.
- Wire: `GET /wamn_control/app/mine` and `PUT /wamn_control/app/default`, their OpenAPI, and the `wamn_control_org` SDK.
- Behaviour: Platform UI opens the default, the only app or the chooser after sign-in, in place of the org console.
- Images at E6.8: `control` and the Platform UI bundle. No `identity` rebuild.
- Tests: control route tests for the list filter and the setting refusal, a live SQL test of the list query, Platform UI unit tests for the four cases of section 4, and one journey on kind that sets a default and reads it back through the routes.

## Questions for rulings

1. Storage: two nullable columns on `identity.org_membership` (A)?
2. List and setting: `GET /wamn_control/app/mine` over every env of the org, and `PUT /wamn_control/app/default`, with the wire of section 2 (A)? Eligibility is project access plus an active app with a browser route.
3. Delivery: Platform UI at the host root, with the console linked for org administrators (A)?
4. One eligible app opens at once, and no eligible app shows a message (section 4)?
5. With no prod env, the apex lists the env-host apps as links (A)?
6. Active role: no change, derived by the host per project (A)?
