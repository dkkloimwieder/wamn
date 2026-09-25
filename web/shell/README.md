# App shell

`@wamn/shell` is the hand-written part of every web application that the generator does not write.
It signs in, keeps the session, lays out the page and routes the address to a screen.
The application gives it a title and its screens, and writes each screen from its generated components.
The [app shell plan](../../docs/plan/app-shell.md) holds the owner rulings.

## The address

The first segment of every address is the environment, as its audience, for example `/urn:wamn:project-env:acme:widgets:dev:k3m9x2p7/pallets`.
A reload renews the session from the renewal cookie, so the page keeps nothing in browser storage.

| Address | Page |
| --- | --- |
| `/` | Sign in. The account lists the environments it can reach, and the chosen one opens its first screen. |
| `/<audience>` | The first screen of the application. |
| `/<audience>/<path>` | One screen. With no session, the page asks for the password on the same address and then shows the screen. |
| Any other address | A page that says no page is there. |

## Use it in an application

An application web page lives in `apps/<app>/web/`, and the generator never writes it.
Its route table is a list of sections, and each section is a list of screens:

```tsx
const SECTIONS: readonly ShellSection[] = [
  {
    label: "Pallets",
    screens: [
      { path: "pallets", label: PalletQueryTableLabel, component: (props) => <PalletQueryTable transport={props.transport} /> },
    ],
  },
];
```

A section label names the model and is written by hand.
A screen label is the label constant that the generated module exports, so no label is written twice.
If a model has one screen, the navigation shows one entry with the model name.
If a model has more than one screen, the navigation shows the model name with the screen labels below it.
The shell renders a screen with one prop, `transport`, the transport of the signed-in session.
That transport sends every API call under `/api`.

The page mounts `<Shell title=... sections=... />` inside `ColorModeProvider`, beside one `Toaster`.
The layout comes from `AppFrame` and `CardPage` in `@wamn/ui`, so the shell states no class.

## The dev server

`vite.ts` in this package exports `applicationConfig`, which an application's `vite.config.ts` imports by path and spreads beside its own plugins.
It resolves the shell, the runtime, the UI and the generated client by path, and it maps `solid-js` and the router to one copy.
The dev server carries `/password` to the identity process, and `/api` to the release without the `/api` prefix.
The edge proxy of a deployment does the same, so one rule holds in both.
An application runs Vite 7, as `web/demo` does. On Vite 8 the page loaded two builds of `solid-js/store` through Kobalte, and it threw before it rendered.

The dev server reads two variables.
`WAMN_DEV_ENV_DIR` names the environment directory that holds `dev.json`.
`WAMN_ROUTE_URL` is the base URL that the dev loop printed.

## Check it

The commands are in [running tests](../../docs/operations/running-tests.md#app-shell).
