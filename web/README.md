# Web

Hand-written TypeScript that generated browser clients import.
The generator writes the bindings, and this directory holds what the bindings call.

| Path | Owner |
| --- | --- |
| [runtime](runtime/) | `@wamn/web-runtime`: the wire contract, and the transport that classifies one response |
| [ui](ui/README.md) | `@wamn/ui`: the copied Zaidan components, the theme, and the exports that generated components render through |
| [components](components/README.md) | The harness that type checks and tests generated components |
| [shell](shell/README.md) | `@wamn/shell`: sign in, the session, the layout and the routes that every application web page shares |

## Runtime

The package is TypeScript source and it compiles nothing.
A bundler reads it directly, and plain Node does not.

To type check it, run:

```bash
cd web/runtime && tsc --project tsconfig.json
```

The generated bindings import this package by name.
An application resolves that name through its own configuration.
The generator's own check maps the name to this directory.
See [running tests](../docs/operations/running-tests.md) for that command.
