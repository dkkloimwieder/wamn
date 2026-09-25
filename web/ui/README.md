# Platform UI

`@wamn/ui` is the one UI package that generated components render through.
It holds the Zaidan components, one theme with its dark mode, and the platform exports that the generator names.
The generator writes no class and no Zaidan code, so all styling lives here.

## The copied source

The components are Zaidan source, copied into this package and owned by the platform.
They are not a dependency, and no registry reads them at run time.
Edit them here like any other platform source.

The copy was taken on 2026-09-22 from the registry `https://zaidan.carere.dev/r/kobalte/{name}.json`, through `shadcn@latest add`.
On 2026-09-23 the design system moved from vega to the preset `buIovdQ`, through `shadcn@latest add @zaidan/preset-buIovdQ`.
That preset replaced `src/styles/base.css`, changed the primary, secondary and sidebar colors, and added the Inter font.
It changed no component source.
By owner direction the font is Fira Code, over the system monospace font, in place of the preset's Inter.
Labels, column headers, buttons, card titles and detail terms read in capitals, and values and descriptions keep their case.

| Kind | Items |
| --- | --- |
| Block | `data-grid`, trimmed to `data-grid.tsx`, `data-grid-table.tsx`, and an index of those two |
| Components | `alert-dialog`, `badge`, `button`, `card`, `checkbox`, `combobox`, `field`, `input`, `input-group`, `label`, `select`, `separator`, `skeleton`, `spinner`, `textarea`, `toast` |
| Shared | `color-mode` |
| Design system | `preset-buIovdQ`: `style-lyra`, `neutral`, the indigo theme, `font-inter`, the default radius |

The copy changed these things:

- Every `@/` import became a relative path, so the package compiles inside any consumer.
- Seven optional props gained `| undefined`, so the source passes `exactOptionalPropertyTypes`.
- `ComboboxContent` gained a `footer` slot below the list, for a next page control.
- `Toaster` gives every toast the `z-toast` class, which the registry item leaves out. `src/styles.css` sets the toast corner through `--border-radius`, because solid-sonner draws it from that variable in CSS outside every layer.
- `DataGridContainer` has a fixed height of 32rem and scrolls in both directions, and the header row is sticky by default. A read never changes the height of the page.
- `src/lib/utils.ts` is the usual `cn`, because the CLI writes it only at `init`.
- `src/styles.css` imports `tw-animate-css`, which `init` also adds, and names this package as its Tailwind source.
- `src/styles.css` carries the shadcn base layer, which gives the page body the theme colors. No registry item writes it.

To add an item, run the CLI in a scratch Vite SolidJS project with the Zaidan `components.json`.
Then copy the new files here and make the same changes.
Add an item only when an emitter target, or a page that places the generated components, needs it.

## The platform exports

| Export | Purpose |
| --- | --- |
| `gridFeatures`, `GridFeatures` | The TanStack Table features every generated table declares |
| `RecordSelect` | The selector: the rows a list returned, a search after a pause in typing, and a next page button |
| `createRecordLabels` | The text of the records a table column names by key, read once for each key |
| `announceOutcome` | Shows one runtime outcome as a toast |
| `TextField`, `ChoiceField`, `CheckField` | One labeled control and the refusal that marks it |
| `DetailList`, `DetailItem` | The fields of one record, with a skeleton while it is read |
| `ConfirmAction` | One action the operator confirms first, in an alert dialog |
| `FormActions` | The buttons that close a form or a table, in one full-width row aligned right |
| `TableScreen` | One table screen: its filter form, its rows and its next page, stacked with one gap |

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
To see every export with sample data, serve the [component gallery](../components/README.md#gallery).
