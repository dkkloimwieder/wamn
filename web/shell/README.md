# App shell

`@wamn/shell` is the hand-written part of every web application that the generator does not write.
It signs in, keeps the session, lays out the page and routes the address to a screen.
The application gives it a title and its screens, and writes each screen from its generated components.
[Execution](../../docs/architecture/execution.md) states the rules that the shell keeps.

## The address

The first segment of every address is the audience: an environment, for example `/urn:wamn:project-env:acme:widgets:dev:k3m9x2p7/pallets`, or the Control of the org, `/urn:wamn:control:acme`.
A reload renews the session from the renewal cookie, so the page keeps nothing in browser storage.

| Address | Page |
| --- | --- |
| `/` | Sign in. The account lists the environments it can reach, and Control when it administers the org or a project. The chosen one opens its first screen. An account that reaches none sees only "No access has been granted.". |
| `/invite#<code>` | The first password. The invitation mail links here, and the page then opens sign in. |
| `/recover` | The sign in page links here. The page asks identity to mail a reset link to the email. |
| `/reset#<code>` | A new password. The reset mail links here, and the page then opens sign in. |
| `/<audience>` | The first screen of the application. |
| `/<audience>/<path>` | One screen. With no session, the page asks for the password on the same address and then shows the screen. |
| `/<audience>/<path>/<id>` | One record page, for example `/<audience>/pallets/<id>`. The navigation entry of its model stays active. |
| `/<audience>/<path>?<query>` | One form, for example `/<audience>/inventory/move?value.palletId=<id>`. The query holds the values that a row filled. |
| `/<audience>/administration/roles` | The role grid of the application. Only `admin` sees it. |
| `/<audience>/administration/users` | The user grid of the application. Only `admin` sees it. |
| `/urn:wamn:control:<org>/org` | The org screen of Control. Only `org-admin` sees it. |
| `/urn:wamn:control:<org>/projects/<project>` | The project screen of one project in Control. Only a caller whose `control.mine` names the project sees it. |
| Any other address | A page that says no page is there. An address of a screen that the caller does not hold shows the same page. |

## Use it in an application

An application web page lives in `apps/<app>/web/`, and the generator never writes it.
Its route table is a list of sections, and each section is a list of screens and other routes:

```tsx
import { PALLET_CREATE_ROUTE, PALLET_GET_ROUTE, PALLET_QUERY_ROUTE } from "@wamn/wms-client/pallet.js";

const pallet = () => import("@wamn/wms-client/components/pallet.js");

const SECTIONS: readonly ShellSection[] = [
  {
    label: "Pallets",
    screens: [
      {
        path: "pallets",
        operation: PALLET_QUERY_ROUTE.operation,
        label: PalletQueryTableLabel,
        actions: [{ label: PalletCreateFormLabel, path: "pallets/new", operation: PALLET_CREATE_ROUTE.operation }],
        component: screen(pallet, (m) => (props) => (
          <m.PalletQueryTable
            transport={props.transport}
            onOpen={{
              "wamn-wms:pallet/get": (row) => props.open(`pallets/${encodeURIComponent(row.id)}`),
            }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "pallets/new",
        operation: PALLET_CREATE_ROUTE.operation,
        component: screen(pallet, (m) => (props) => (
          <m.PalletCreateForm
            transport={props.transport}
            onSubmitted={(outcome) => outcome.status === "completed" && props.close()}
          />
        )),
      },
      {
        path: "pallets/:id",
        operation: PALLET_GET_ROUTE.operation,
        component: screen(pallet, (m) => (props) => (
          <m.PalletGetDetail transport={props.transport} input={{ id: props.params.id ?? "" }} />
        )),
      },
    ],
  },
];
```

A section label names the model and is written by hand.
A screen label is a constant from the generated `components/labels.js`, so no label is written twice.
The route table imports the labels and the route constants. Each route component loads its generated model module through `screen(load, pick)`, so a screen loads its code only when it opens.
If a model has one screen, the navigation shows one entry with the model name.
If a model has more than one screen, the navigation shows the model name with the screen labels below it.
A record page or a form has no navigation entry.
A table row opens a record page or a form through a row callback of the generated table.
A row that fills a form opens it with `fillPath(path, values)`, which writes each filled member into the query, such as `value.palletId`.
The form route reads them back with `filledValues(props.search)`, so a reload keeps them.
Each screen, route and action names its operation from the generated route constant, such as `PALLET_GET_ROUTE.operation`.
After sign in, the shell reads `permission.mine` once, and it shows only the screens, routes and actions of the operations that the caller holds.
`admin` holds every operation. The shell compares a reference without its version, so `wamn-wms:pallet/get@0.1.0` matches `wamn-wms:pallet/get`.
This is presentation only. Every route repeats its own check, and a refusal still shows its contract text.
A route can list `actions`, which the shell shows as buttons above the route.
An action path can name a route parameter, such as `:id` in `locations/:id/update`, and the shell fills it from the address.

The shell owns the router, and a screen never imports it.
The shell renders a screen with five props:

| Prop | Value |
| --- | --- |
| `transport` | The transport of the signed-in session. It sends every API call under `/api`. |
| `params` | The values in the address, by the names in the route path, for example `id` for `pallets/:id`. |
| `search` | The values in the query of the address, for example `value.palletId`. |
| `open(path)` | Opens a path below the environment, with a query if the caller gives one. The caller encodes each value that it puts in the path. |
| `close()` | Returns to the page that opened this one. If no page opened it, for example after a pasted address, it opens the first screen of the section. |

The page mounts `<Shell title=... sections=... />` inside `ColorModeProvider`.
The layout comes from `AppFrame`, `CardPage` and `ScreenActions` in `@wamn/ui`, so the shell states no class.
The sign in page loads alone. The shell loads `layout.tsx`, with `AppFrame` and the `Toaster`, only after sign in.
The router root is one Suspense boundary, so a page paints when its layout and screen modules are loaded, and a navigation keeps the old screen until the new one loads.

## Administration and Control

The shell adds an Administration section to every application, and only `admin` sees it.
It holds `RoleGrid` and `UserGrid` from `@wamn/ui/admin`, which call the application route set under `/api/wamn_control`.

Control is the audience `urn:wamn:control:<org>` of the shell's org, and identity lists it beside the environments for `org-admin` or `project-admin`.
Under Control, the shell reads `control.mine` once and calls the control route set at `/wamn_control`, with no `/api` prefix.
It shows the org screen only to `org-admin`, and one project screen for each project that `control.mine` names.
Both screens come from `@wamn/ui/admin` and load only when they open.

`OrgScreen` lists the members of the org with an `org-admin` toggle and an activate or deactivate button, and its form invites a user.
`ProjectScreen` lists the members of one project with a membership toggle for each environment and a `project-admin` toggle.
A grant that a higher grant covers shows as hierarchy-controlled, with no toggle.
A new member of a project is chosen from the members of the org.
A refused write shows its contract text, and a partial write also names the environments that completed.

## The dev server

`vite.ts` in this package exports `applicationConfig`, which an application's `vite.config.ts` imports by path and spreads beside its own plugins.
It resolves the shell, the runtime, the UI and the generated client by path, and it maps `solid-js` and the router to one copy.
The dev server carries `/password` to the identity process, and `/api` to the release without the `/api` prefix.
The browser needs that proxy to see one origin. The release selects its application by the `Host` header, which a browser cannot set, and the identity process signs its own certificate, which a browser does not trust.
The edge proxy of a deployment does the same, so one rule holds in both.
An application runs Vite 7. On Vite 8 the page loaded two builds of `solid-js/store` through Kobalte, and it threw before it rendered.

The dev server reads two variables.
`WAMN_DEV_ENV_DIR` names the environment directory that holds `dev.json`.
`WAMN_ROUTE_URL` is the base URL that the dev loop printed.

## Check it

The commands are in [running tests](../../docs/operations/running-tests.md#app-shell).
`test/browser` holds the page and the Chrome journey of the browser test, which `shell_browser_live` serves.
