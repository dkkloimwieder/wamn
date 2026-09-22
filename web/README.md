# Web

Hand-written TypeScript that generated browser clients import.
The generator writes the bindings, and this directory holds what the bindings call.

| Path | Owner |
| --- | --- |
| [runtime](runtime/) | `@wamn/web-runtime`: the wire contract, and the transport that classifies one response |
| [components](components/README.md) | The harness that type checks and tests generated components |
| [demo](demo/README.md) | Disposable page that runs the generated Receiving components against a local stack |

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
