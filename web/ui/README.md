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
On 2026-09-25 the app shell added `sidebar` from the same registry, with the items it needs that the copy lacked: `sheet`, `tooltip` and `use-mobile`.
On 2026-09-25 the data table added `popover` from the same registry, for its column filters.
On 2026-09-25 the data table added `dropdown-menu` and `switch` from the same registry, for its header menu and its column panel.
By owner direction the font is Fira Code, over the system monospace font, in place of the preset's Inter.
Labels, column headers, buttons, card titles and detail terms read in capitals, and values and descriptions keep their case.

| Kind | Items |
| --- | --- |
| Components | `alert-dialog`, `badge`, `button`, `card`, `checkbox`, `combobox`, `dropdown-menu`, `field`, `input`, `input-group`, `label`, `popover`, `select`, `sheet`, `sidebar`, `skeleton`, `spinner`, `switch`, `toast`, `tooltip` |
| Hook | `use-mobile` |
| Shared | `color-mode` |
| Design system | `preset-buIovdQ`: `style-lyra`, `neutral`, the indigo theme, `font-inter`, the default radius |

The copy changed these things:

- Every `@/` import became a relative path, so the package compiles inside any consumer.
- Twenty optional props gained `| undefined`, so the source passes `exactOptionalPropertyTypes`.
- For the same reason, the mobile sidebar states the type of the props it spreads onto `Sheet`.
- `tooltip` is a thin wrapper over the Kobalte tooltip, in place of the registry item, which rebuilt Base UI's tooltip on top of Kobalte. It keeps the item's classes, its 600 ms open delay and its padding of 5 pixels, and it carries the side Kobalte settled on as `data-side`.
- `ComboboxContent` gained a `footer` slot below the list, for a next page control.
- `toast` is the platform's own toast in place of the registry item, which wraps solid-sonner. `Toaster` shows each toast for four seconds at the top center, with the item's icons, and gives it the `z-toast` class.
- `src/lib/utils.ts` has a `cn` that joins classes with `clsx` and does not use `tailwind-merge`. A default that a caller overrides, such as the width of `field`, `input` and `input-group`, moved from the component into its `z-` rule in the base layer. The caller's utility class then wins.
- Each `lucide-solid` icon and the Kobalte `polymorphic` import come from their own paths.
- `src/styles.css` imports `tw-animate-css`, which `init` also adds, and names this package as its Tailwind source.
- `src/styles.css` carries the shadcn base layer, which gives the page body the theme colors. No registry item writes it.
- The `data-grid` block is gone (wamn-5pzt). `src/table/grid.tsx` keeps the classes of the path the tables used, in the platform's own grid.
The copy keeps only what something imports. A component part, a helper or a type that no file names is removed. A rule in `src/styles/base.css` is removed when no source names its `z-` class, and `src/styles/utilities.css` keeps only `no-scrollbar`. The `textarea` item went because nothing used it.

To add an item, run the CLI in a scratch Vite SolidJS project with the Zaidan `components.json`.
Then copy the new files here and make the same changes.
If the CLI writes `src/styles/base.css`, remove again the rules that no source names.
Add an item only when an emitter target, or a page that places the generated components, needs it.

## The platform exports

| Export | Purpose |
| --- | --- |
| `RecordSelect` | The selector: the rows a list returned, a search after a pause in typing, a next page button, and the one record a stored value names when the list did not return it. It reports the row that carries the stored value, so a form reads that row's revision |
| `createRecordLabels` | The text of the records a table column names by key, read once for each key and again after each write |
| `announceOutcome` | Shows one runtime outcome as a toast |
| `TextField`, `ChoiceField`, `CheckField` | One labeled control and the refusal that marks it |
| `DetailList`, `DetailItem` | The fields of one record, with a skeleton while it is read |
| `ConfirmAction` | One action the operator confirms first, in an alert dialog |
| `FormActions` | The buttons that close a form or a table, in one full-width row aligned right |
| `FormDone` | The line a form shows beside its buttons after its command completes |
| `TableScreen` | One table screen. A `QueryTable` in it takes the height of the viewport, and at least 32rem |
| `QueryTable` | The table of one generated read, wired from its table definition alone: the load, the scope filters, the sort, the cap, the record labels, the row buttons, the inline edit, the bulk actions through their forms, the child tables, the column arrangement, the views and the URL. Over a fully read set it places a `SetTable` |
| `SetTable` | Every row of a complete set: refine filters, search, a client sort, grouping by day, week or month, aggregates, a totals row and CSV export. It takes its rows, its columns and its state from its caller |
| `builtColumns`, `defaultSetView`, `defaultGridView` | The columns and the first state of a `SetTable` that a page places alone |
| `WINDOW_FROM`, `ROW_HEIGHT` | A table windows its rows above `WINDOW_FROM` rows inside its box. Every row is `ROW_HEIGHT` pixels tall |
| `AppFrame` | The signed-in page of an application: the sidebar with its navigation, a header, and the screen |
| `CardPage` | One card in the middle of an empty page, for signing in and for an address with no page |
| `ScreenActions` | The row of buttons above a screen |

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
The sidebar stores whether it is open in the cookie `sidebar_state`, which holds `true` or `false`.

## Check it

The commands are in [running tests](../../docs/operations/running-tests.md#platform-ui).
To see every export with sample data, serve the [component gallery](../components/README.md#gallery).
