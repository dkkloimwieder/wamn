import type { OperationRoute, Transport } from "@wamn/web-runtime";
import * as client from "./generated/wamn_receiving/index.js";
import type { ReferencePresentation } from "./generated/wamn_receiving/components/index.js";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";

const routes = Object.values(client).flatMap((model) =>
  Object.entries(model)
    .filter(([name]) => name.endsWith("_ROUTE"))
    .map(([, route]) => route as OperationRoute),
);

export function referenceRoute(operation: string): OperationRoute {
  const route = routes.find((route) => route.operation === operation);
  if (!route) throw new Error(`No route for ${operation}`);
  return route;
}

/** Keep reference lookups shared within one screen load. */
export function referenceLabels(transport: Transport) {
  const lists = new Map<string, Promise<readonly RecordValue[]>>();
  const records = new Map<string, Promise<RecordValue>>();
  return async (reference: ReferencePresentation, key: unknown, input: RecordValue = {}): Promise<string> => {
    if (key === null || key === undefined || key === "") return "";
    let row: RecordValue | undefined;
    if (reference.read) {
      const read = reference.read;
      const cacheKey = JSON.stringify([read.operation, key]);
      let pending = records.get(cacheKey);
      if (!pending) {
        pending = invoke<RecordValue>(transport, referenceRoute(read.operation), { [read.keyInput]: String(key) });
        records.set(cacheKey, pending);
      }
      row = await pending;
    } else {
      const cacheKey = JSON.stringify([reference.operation, input]);
      let pending = lists.get(cacheKey);
      if (!pending) {
        pending = (async () => {
          const rows: RecordValue[] = [];
          let cursor: string | null | undefined;
          do {
            const page = await invoke<Rows>(transport, referenceRoute(reference.operation), {
              ...input,
              ...(cursor && reference.cursorInput ? { [reference.cursorInput]: cursor } : {}),
            });
            rows.push(...rowsOf(page));
            cursor = reference.cursorInput ? page.next_cursor : undefined;
          } while (cursor);
          return rows;
        })();
        lists.set(cacheKey, pending);
      }
      row = (await pending).find((row) => String(row[reference.keyField]) === String(key));
    }
    return row?.[reference.displayField] == null ? "Unavailable" : String(row[reference.displayField]);
  };
}

export async function displayRecord(
  row: RecordValue,
  references: Readonly<Record<string, ReferencePresentation>>,
  label: ReturnType<typeof referenceLabels>,
): Promise<RecordValue> {
  const fields = await Promise.all(
    Object.entries(references).map(async ([field, reference]) => [
      field,
      await label(reference, row[field]).catch(() => "Unavailable"),
    ]),
  );
  return { ...row, ...Object.fromEntries(fields) };
}
