# Platform UI application — spec

Server paging, `wamn-n8q9m.20`, remains open.
[Execution](../architecture/execution.md#operator-clients) records current client and shell behavior.

## 1. Goal and scope

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

## 2. Decisions (proposed)

| # | Decision | Why |
| --- | --- | --- |
| D1 | wfe is the only UI dependency. `@wfe/components`, `@wfe/form`, `@wfe/generator`, Solid 2 pinned at exactly `2.0.0-rc.14`. | One component owner. wamn `web/ui` and `web/components` are deleted. |
| D3 | wfe stays a separate repo. wamn consumes it by pinned git revision; tag when one exists. Generated clients are captured into wfe only for wfe's own examples. | wfe has no WAMN imports and must stay that way. |

## 3. Audiences and access

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

## 4. Screens

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

## 5. Splash tiles

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

## 6. wfe dependency

wamn consumes wfe by pinned git revision; tag when one exists. The pin is wfe `main` at `45faa4aed7de92a7cd3e0a0b772bbd845e52ba63`. The owner tags wfe after the wfe shell work, and the pin then moves to that tag. The platform UI needs these wfe exports: `@wfe/components` (with `layout` and `table`), `@wfe/form`, `@wfe/generator` (`TableDescription`, `FormDescription`, `GridDescription`, `LayoutView`), `@wfe/shell`, `StatusTile`, `TileRow`, and `label`/`format` on descriptions.

The wfe side is its own plan in the wfe repository. M1 starts on the exports that exist at the pin. Work that needs `@wfe/shell`, `GridDescription`, `StatusTile`, `TileRow` or `label`/`format` waits for the wfe tag, with one blocking bead per missing export. A wamn issue that finds a wfe gap files it against wfe and stops; it does not patch around it in wamn.

## 7. wamn track

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

## 8. What stays out of the platform UI

See [browser client and shell behavior](../architecture/execution.md#operator-clients).

- Appearance review controls, mock scenarios, SSR hydration cases. These belong to wfe examples.
- Any client-side authorization rule. The client hides; the server refuses.
- Browser storage for anything but the session cookie (`__Host-wamn-session`).
- Traffic metrics. A named bead after M5.
