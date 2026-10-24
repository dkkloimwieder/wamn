import { createMemo, onCleanup, type Accessor } from "solid-js";
import { count, createLiveQueryCollection, sum } from "@tanstack/db";
import { liveRows, syncStatus, type Row } from "@wfe/sync";
import type { packagingCollection } from "./packaging-feed";
import { orderedIds, type StatusFilter } from "./order";
import type { GroupRow, ViewSource } from "./view";

/**
 * wfe finding (ii): `liveRows` accepts only a collection of rows with `id` and
 * `row_version`. A live query result has neither, so this cast reads it.
 */
const queryRows = <T>(result: unknown) => liveRows(result as Parameters<typeof liveRows<T & Row>>[0]);

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
  // because row_version is int32 and the total stays below 2^53.
  const grouped = createLiveQueryCollection((q) =>
    q
      .from({ p: c })
      .groupBy(({ p }) => [p.status, p.type])
      .select(({ p }) => ({ status: p.status, type: p.type, count: count(p.id), total: sum(p.rowVersion) })),
  );
  const whole = createLiveQueryCollection((q) =>
    q.from({ p: c }).select(({ p }) => ({ count: count(p.id), total: sum(p.rowVersion) })),
  );
  onCleanup(() => {
    void grouped.cleanup();
    void whole.cleanup();
  });
  const groupRows = queryRows<GroupRow>(grouped);
  const totalRows = queryRows<{ count: number; total: number }>(whole);
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
