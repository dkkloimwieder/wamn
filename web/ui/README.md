# Platform UI

`@wamn/ui` is the one UI package that generated components render through.
It holds the Zaidan components, one theme with its dark mode, and the platform exports that the generator names.
The generator writes no class and no Zaidan code, so all styling lives here.

## The copied source

The components are Zaidan source, copied into this package and owned by the platform.
They are not a dependency, and no registry reads them at run time.
Edit them here like any other platform source.

The copy was taken on 2026-09-22 from the registry `https://zaidan.carere.dev/r/kobalte/{name}.json`, through `shadcn@latest add`.

| Kind | Items |
| --- | --- |
| Block | `data-grid`, trimmed to `data-grid.tsx`, `data-grid-table.tsx`, and an index of those two |
| Components | `alert-dialog`, `badge`, `button`, `checkbox`, `combobox`, `field`, `input`, `input-group`, `label`, `select`, `separator`, `skeleton`, `spinner`, `textarea`, `toast` |
| Shared | `color-mode` |
| Design system | `style-vega`, `neutral`, `radius-medium` |

The copy changed these things:

- Every `@/` import became a relative path, so the package compiles inside any consumer.
- Seven optional props gained `| undefined`, so the source passes `exactOptionalPropertyTypes`.
- `ComboboxContent` gained a `footer` slot below the list, for a next page control.
- `src/lib/utils.ts` is the usual `cn`, because the CLI writes it only at `init`.
- `src/styles.css` imports `tw-animate-css`, which `init` also adds, and names this package as its Tailwind source.

To add an item, run the CLI in a scratch Vite SolidJS project with the Zaidan `components.json`.
Then copy the new files here and make the same changes.
Add an item only when an emitter target needs it.

## The platform exports

| Export | Purpose |
| --- | --- |
| `gridFeatures`, `GridFeatures` | The TanStack Table features every generated table declares |
| `RecordSelect` | The selector: the rows a list returned, a search after a pause in typing, and a next page button |
| `announceOutcome` | Shows one runtime outcome as a toast |
| `TextField`, `ChoiceField`, `CheckField` | One labeled control and the refusal that marks it |
| `DetailList`, `DetailItem` | The fields of one record, with a skeleton while it is read |
| `ConfirmAction` | One action the operator confirms first, in an alert dialog |

`RecordSelect` filters nothing itself, so its options are exactly the rows the release sent.
A search matches a value in full, because a declared filter compares with `IN`.

## Use it in an application

Import the stylesheet once, and build it with `@tailwindcss/vite`:

```ts
import "@wamn/ui/styles.css";
```

Wrap the page in `ColorModeProvider` and mount one `Toaster`.
Resolve `@wamn/ui` by path, the same way as `@wamn/web-runtime`.
Map `solid-js` and `@tanstack/solid-table` to one copy, because two copies of `solid-js` break context and reactivity.

The color mode is stored in the cookie `zaidan-color-mode`.
It holds `light` or `dark` and nothing else.

## Check it

The commands are in [running tests](../../docs/operations/running-tests.md#platform-ui).
