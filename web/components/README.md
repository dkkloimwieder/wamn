# Component check

The harness that type checks and tests generated components.
It is hand-written, private, and it ships nothing.

The generated components import SolidJS, TanStack Table, TanStack Form, and zod.
This package installs those libraries once, so a check needs no network.

Install the dependencies:

```bash
cd web/components && npm install
```

Write the fixture bindings and components, then check them:

```bash
cargo run --locked --offline -p wamn-schema-generator --example check_client_components
```

The command writes into `fixture/`, which Git ignores, and then runs the check in this package.
`npm run check` and `npm test` read what the command wrote, so run the command first.
