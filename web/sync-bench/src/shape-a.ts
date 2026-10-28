import { createMemo, onCleanup, type Accessor } from "solid-js";
import { count, createLiveQueryCollection, multiply, sum } from "@tanstack/db";
import { liveRows, syncStatus } from "@wfe/sync";
import type { packagingCollection } from "./generated/wamn_wms/packaging.js";
import { orderedIds, type StatusFilter } from "./order.js";
import type { ViewSource } from "./view.js";

/**
 * Shape A: the rows through `liveRows` on the collection, and the groups and
 * totals from `@tanstack/db` live queries read through `liveRows`.
 */
export function shapeA(
  c: ReturnType<typeof packagingCollection>,
  filter: Accessor<StatusFilter>,
): ViewSource & { status: Accessor<"loading" | "ready" | "error"> } {
  const rows = liveRows(c);
  // wfe finding (iii): the built-in sum is a float sum. It is exact here only
  // because the synthetic versions and their total stay below 2^53.
  const grouped = createLiveQueryCollection((q) =>
    q
      .from({ p: c })
      .groupBy(({ p }) => [p.status, p.type])
      .select(({ p }) => ({
        status: p.status,
        type: p.type,
        count: count(p.id),
        total: sum(multiply(p.rowVersion, 1)),
      })),
  );
  const whole = createLiveQueryCollection((q) =>
    q.from({ p: c }).select(({ p }) => ({ count: count(p.id), total: sum(multiply(p.rowVersion, 1)) })),
  );
  onCleanup(() => {
    void grouped.cleanup();
    void whole.cleanup();
  });
  const groupRows = liveRows(grouped);
  const totalRows = liveRows(whole);
  const ids = createMemo(() => orderedIds(rows, filter()));
  const groups = createMemo(() =>
    Object.values(groupRows).sort((left, right) =>
      left.status === right.status ? (left.type < right.type ? -1 : 1) : left.status < right.status ? -1 : 1,
    ),
  );
  return {
    ids,
    get: (id) => rows[id],
    groups,
    totals: () => Object.values(totalRows)[0] ?? { count: 0, total: 0 },
    status: syncStatus(c),
  };
}
