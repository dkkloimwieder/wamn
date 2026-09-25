# App shell

Epic 18 builds the app shell. The app shell is the hand-written part of a web application that the generator does not write. Beads epic `wamn-d0zc` holds the issues and their status. The owner reviewed this scope on 2026-09-25.

## 1. Goal

1. A platform package, `web/shell`, signs in, keeps the session, lays out the page and navigates between screens.
2. WMS and Receiving each get a web application that places their generated components on routes through that package.
3. The disposable `web/demo` page goes away when both applications run every screen that it runs.

The [web operator client](web-operator-client.md) plan names this part in its shape: "App. Routes, login, navigation, screens. Not generated." Epic 17 ended with the instruction to scope it.

## 2. Fixed rules

These rules come from earlier owner decisions, and this epic keeps them.

- The generator writes no shell code. The shell and the screens are hand-written, and screens belong to the agent that writes the application.
- Generated files are not edited.
- The session is the cookie carrier of Epic 11. The browser holds no token and writes nothing to browser storage.
- All styling lives in `web/ui`. The shell renders through its exports and adds no theme of its own.
- The browser sees one origin. The local dev loop uses a Vite proxy, and a later epic puts an edge proxy in front of a static bucket.
- An emitter change regenerates every application that declares `client_package` in the same commit.

## 3. Current state

Measured on main at `f2ce018ea` on 2026-09-25.

| Place | Today |
| --- | --- |
| `web/demo` | One page with no router. It signs in, then mounts every component of one application in cards. `WAMN_DEMO_APP` selects Receiving or WMS when Vite starts. |
| `web/demo/src/app.tsx` | Sign in, the session keeper, and the environment in the address fragment, so a reload renews the session. |
| `web/demo/src/wms.tsx` | 306 lines that hold one selected record of each model, and pass it from a table row to the detail and the forms. |
| `web/demo/vite.config.ts` | Carries `/password` to the issuer, and a hand-written list of path prefixes to the release. WMS has six prefixes. |
| Generated `components/index.ts` | Exports each component and its screen label constant, for example `PalletQueryTableLabel`. |
| Route templates | Each binding states its template, for example `/pallet/create`. The templates sit at the root of the origin. |
| `web/runtime` | `keepSession`, `environments`, `createTransport` with the cookie carrier, and the outcome sentences. |
| `web/ui` | The Zaidan copy has no `sidebar` and no router. |
| main | Red since `1931d925f`. `wamn-is2k` holds the fix. It belongs to the agent that owns Epic 16, and issue 1 starts when it merges. |

## 4. Decisions

Each decision states the owner ruling of 2026-09-25.

### 4.1 Where the code lives

`web/shell` is one platform package, `@wamn/shell`, that every application shares. Each application has its own Vite application in `apps/<app>/web/`, which holds its route table and its screens. The WMS application depends on the shell, the generated WMS client and `web/ui`.

The application is owned code. The generator never writes or regenerates it.

### 4.2 Router

`@solidjs/router`. It is smaller and native to Solid, and the shell needs only routes and path parameters. Typed search parameters belong to the caching epic, not to the shell.

### 4.3 Page paths and API paths

A page path such as `/pallet` collides with the route template `/pallet/create` on one origin.

The transport sends every API call under `/api`, and the proxy strips that prefix before it reaches the release. The generated templates do not change, because the prefix is the base URL of the transport. The dev proxy then needs one rule in place of a list of prefixes.

The edge proxy of the CDN epic does the same. One rule holds in the dev loop and in deployment.

### 4.4 Navigation

Each application writes its navigation by hand in its route table, grouped by model. The labels come from the exported label constants, so no label is written twice. The shell shows a model with one screen as one entry with the model name. It shows the screen labels only below a model with more than one screen, for example "Pallets" with "query" and "aggregate" below it. The generated component index is what a developer or an agent reads to write that table. The generator emits no navigation until an application needs it.

### 4.5 Record and command screens

Issue 2 built the record routes and issue 3 built the form routes, which [execution](../architecture/execution.md) describes. The key and the filled values in the address replace the selected record and the fills that the demo holds in memory.

### 4.6 The environment in the address

The demo writes the environment into the address fragment, so a reload renews the session with no browser storage. The shell keeps that rule. Issue 1 made the audience the first path segment, for example `/urn:wamn:project-env:acme:widgets:dev:k3m9x2p7/pallets`.

## 5. Issues

Epic 18 (`wamn-d0zc`) closed after issue 3, by owner ruling of 2026-09-25, with WMS usable on the shell. Issues 4 and 5 remain open as their own issues. Each next issue is scoped after the owner reviews the one before it.

1. `wamn-d0zc.1`: the shell package and the WMS application. Sign in and sign out, the layout with a sidebar and a header, one route for each WMS table screen, a not-found route, and the `/api` prefix. Component tests on a stub transport.
2. `wamn-d0zc.2`: WMS record pages. A table row opens `/<path>/<id>`, and the page mounts the get detail from the key in the address.
3. `wamn-d0zc.3`: WMS command forms on their own routes, opened from a table action, a row or a record page, and returning to the opener when they complete.
4. `wamn-xvb5`: Receiving on the shell.
5. `wamn-defe`: delete `web/demo`, and move its run instructions and seed notes to the applications.

The plan is done when WMS and Receiving run every generated component from the shell against a local stack at the 1000 seed, and `web/demo` is gone.

## 6. Out

- Static hosting, the CDN bucket and the edge proxy. The shell must build to static files, but nothing serves them.
- The platform admin contract and the admin UI.
- The terminal client.
- Server rendering and live updates.
- A navigation list that the generator emits.
