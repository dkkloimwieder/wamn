import type { Int64, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import type { Outbox } from "@wfe/sync";
import * as widget from "./fixture/widget.js";
import type * as int32 from "./int32/widget.js";
import type * as int64 from "./int64/widget.js";
import type * as counter from "./fixture/counter.js";
import type * as widened from "./widened/widget.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Expect<T extends true> = T;

export type Contracts = [
  Expect<Equal<widget.WidgetCreateRequest["id"], Uuid>>,
  Expect<Equal<widget.WidgetGetResult["code"], "priority" | "standard">>,
  Expect<Equal<widget.WidgetDeleteResult["outcome"], "deleted" | null>>,
  Expect<Equal<widget.WidgetQueryRequestSort["field"], "created_at">>,
  Expect<Equal<widened.WidgetQueryRequestSort["field"], "created_at" | "code">>,
  Expect<Equal<widget.WidgetQueryRequestSort["direction"], "ascending" | "descending">>,
  Expect<Equal<widget.WidgetQueryRequest["limit"], number | undefined>>,
  Expect<Equal<widget.WidgetQueryRequest["cursor"], string | undefined>>,
  Expect<Equal<widget.WidgetRecordBatchRequestValue["expectedEditVersion"], Int64>>,
  Expect<Equal<widget.WidgetListResult["rows"], readonly widget.WidgetListRow[]>>,
  Expect<Equal<widget.WidgetQueryResult["item"], readonly widget.WidgetQueryRow[]>>,
  Expect<Equal<widget.WidgetQueryResult["nextCursor"], string | null>>,
  Expect<Equal<widget.WidgetQueryResult["cursor"], string>>,
  Expect<Equal<Awaited<ReturnType<typeof widget.query>>, Outcome<widget.WidgetQueryResult>>>,
  Expect<Equal<int32.WidgetQueryRow["id"], number>>,
  Expect<Equal<int32.WidgetQueryChange["id"], number>>,
  Expect<Equal<int64.WidgetQueryRow["id"], Int64>>,
  Expect<Equal<int64.WidgetQueryChange["id"], Int64>>,
  Expect<Equal<int64.WidgetGetRequest["id"], Int64>>,
  Expect<Equal<counter.CounterQueryChange["id"], Int64>>,
  Expect<Equal<Parameters<typeof counter.counterWrite>[1], Outbox<Int64>>>,
  Expect<Equal<Parameters<typeof int32.widgetWrite>[1], Outbox<number>>>,
  Expect<Equal<Parameters<typeof int64.widgetWrite>[1], Outbox<Int64>>>,
  Expect<Equal<Extract<keyof int32.WidgetCreateRequest, "id">, never>>,
  Expect<Equal<Extract<keyof int64.WidgetCreateRequest, "id">, never>>,
];

export function invokeEveryBinding(transport: Transport, signal: AbortSignal) {
  const options = { signal };
  return [
    widget.archive(transport, [] as widget.WidgetArchiveRequest[], options),
    widget.create(transport, [] as widget.WidgetCreateRequest[], options),
    widget.delete_(transport, [] as widget.WidgetDeleteRequest[], options),
    widget.get(transport, [] as widget.WidgetGetRequest[], options),
    widget.list(transport, [] as widget.WidgetListRequest[], options),
    widget.query(transport, [] as widget.WidgetQueryRequest[], options),
    widget.recordBatch(transport, [] as widget.WidgetRecordBatchRequest[], options),
    widget.update(transport, [] as widget.WidgetUpdateRequest[], options),
    widget.changes(transport, {}, "cursor", 10, options),
  ];
}

export function refuseInvalidInputs(transport: Transport, result: widget.WidgetGetResult) {
  // @ts-expect-error A returned record exposes a read-only identity.
  result.id = "replacement";
  // @ts-expect-error The published code domain refuses other strings.
  void widget.create(transport, [{ id: "id", code: "other", idempotencyKey: "k", requestId: "r" }]);
  // @ts-expect-error A page limit is an integer, not an opaque int64.
  void widget.query(transport, [{ limit: "10" }]);
  // @ts-expect-error The declaration admits no undefined value for an optional cursor.
  void widget.query(transport, [{ cursor: undefined }]);
  // @ts-expect-error A JSON value stays opaque and admits no arbitrary runtime object.
  void widget.list(transport, [{ selector: new Date() }]);
}
