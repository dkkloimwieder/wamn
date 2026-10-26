import { createMemo, createSignal, createStore, type Accessor } from "solid-js";
import type { Change, Feed } from "@wfe/sync";
import type { PackagingRow, PackagingScope } from "./packaging-feed.js";
import { orderedIds, type StatusFilter } from "./order.js";
import { exactSum } from "./totals.js";
import type { GroupRow, ViewSource } from "./view.js";

/**
 * Shape B: no `@tanstack/db`. A Solid store keyed by id, filled from the same
 * feed. Sort, filter, groups and totals are memos over the store, recomputed
 * per change. A batch lands in one store write with the insert, update and
 * delete rules of `applyChanges`.
 */
export function shapeB(
  feed: Feed<PackagingRow, PackagingScope>,
  scope: PackagingScope,
  filter: Accessor<StatusFilter>,
): ViewSource & {
  status: Accessor<"loading" | "ready" | "error">;
  apply: (changes: readonly Change<PackagingRow>[]) => void;
} {
  const [rows, set] = createStore<Record<string, PackagingRow>>({});
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const apply = (changes: readonly Change<PackagingRow>[]) =>
    set((draft) => {
      for (const change of changes) {
        const stored = draft[change.id];
        if (change.deleted) {
          if (stored) delete draft[change.id];
        } else if (!stored || change.row_version > stored.rowVersion) draft[change.id] = change.data;
      }
    });
  feed.list(scope).then(
    (page) => {
      set((draft) => {
        const incoming = new Set<string>();
        for (const row of page.rows) {
          incoming.add(row.id);
          const stored = draft[row.id];
          if (!stored || row.rowVersion > stored.rowVersion) draft[row.id] = row;
        }
        for (const id of Object.keys(draft)) if (!incoming.has(id)) delete draft[id];
      });
      setStatus("ready");
    },
    () => setStatus("error"),
  );
  const ids = createMemo(() => orderedIds(rows, filter()));
  const groups = createMemo(() => {
    const found = new Map<string, { status: string; type: string; versions: number[] }>();
    for (const row of Object.values(rows)) {
      const key = `${row.status}|${row.type}`;
      let group = found.get(key);
      if (!group) found.set(key, (group = { status: row.status, type: row.type, versions: [] }));
      group.versions.push(row.rowVersion);
    }
    return [...found.values()]
      .map((group): GroupRow => ({
        status: group.status,
        type: group.type,
        count: group.versions.length,
        total: exactSum(group.versions),
      }))
      .sort((left, right) =>
        left.status === right.status ? (left.type < right.type ? -1 : 1) : left.status < right.status ? -1 : 1,
      );
  });
  const totals = createMemo(() => {
    const all = Object.values(rows);
    return { count: all.length, total: exactSum(all.map((row) => row.rowVersion)) };
  });
  return { ids, get: (id) => rows[id], groups, totals, status, apply };
}
