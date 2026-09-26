# Component check

The harness that type checks and tests generated components.
It is hand-written, private, and it ships nothing.

The generated components import SolidJS, TanStack Table, TanStack Form, zod, and `@wamn/ui`.
This package installs those libraries, so a check needs no network.
It resolves `@wamn/ui` from `web/ui`, and it maps `solid-js` and the table package to its own copy, because two copies break reactivity.

Install the web packages once with `pnpm install` at the repository root, as [running tests](../../docs/operations/running-tests.md#web-packages) describes.

Write the fixture bindings and components, then check them:

```bash
cargo run --locked --offline -p wamn-schema-generator --example check_client_components
```

The command writes into `fixture/`, which Git ignores, and then runs the check in this package.
`pnpm run check` and `pnpm test` read what the command wrote, so run the command first.

## Gallery

The gallery is one page that shows every `@wamn/ui` export and every generated screen of the platform fixture.
It uses sample data, needs no stack, and makes no request outside its own server.
The page reads the same stub transports as the tests, from `stubs/index.ts`.
It reads the platform fixture only, never an application.

Write the fixture, then serve the page on a port that you choose:

```bash
cargo run --locked --offline -p wamn-schema-generator --example check_client_components
cd web/components && pnpm run gallery --port 5191
```

If `fixture/` is absent, the command exits and names the fixture command.
The switch at the top of the page changes between light and dark mode.

The gallery leaves out no component or state.
You see two states by using the control: the open `ConfirmAction` dialog, and a `RecordSelect` search.

## Visual test

The visual test draws each gallery section in light mode and in dark mode, and compares it with its reference image.
The reference images are in `gallery/__screenshots__/gallery.visual.tsx/`, one PNG file for each section and mode.
A section fails when one pixel changed. The comparison allows no color difference and counts anti-aliased pixels.
The test runs in Vitest browser mode, in the installed Chrome, through Playwright, so it downloads no browser.
It stops the clock, so a load time or a date draws the same on each run.
The images come from Chrome on Linux, so another browser version or font set can draw a different pixel.

Write the fixture, then run the test:

```bash
cargo run --locked --offline -p wamn-schema-generator --example check_client_components
cd web/components && pnpm run test:visual
```

If a section fails, the test writes the actual image and a diff image under `.vitest/`, which Git ignores.
Look at both images.
If the change is intended, accept it:

1. Run `pnpm run test:visual --update` to write new reference images.
2. Run `pnpm run test:visual` again, and make sure that it passes.
3. Commit the changed PNG files with the change that caused them.

A new gallery section gets its reference images the same way.
