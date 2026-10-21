import { refusalSentence, type JsonValue, type OperationRoute, type Outcome, type Transport } from "@wamn/web-runtime";

export type RecordValue = Record<string, JsonValue>;

export class Refusal extends Error {
  constructor(readonly outcome: Exclude<Outcome<unknown>, { status: "completed" }>) {
    super(
      outcome.status === "refused"
        ? refusalSentence(outcome.code, outcome.text)
        : outcome.status === "uncertain"
          ? outcome.reason
          : "The request completed only in part.",
    );
  }
}

export async function invoke<T>(transport: Transport, route: OperationRoute, input: RecordValue): Promise<T> {
  const outcome = await transport.invoke({ ...route, items: [input] });
  if (outcome.status !== "completed") throw new Refusal(outcome);
  return outcome.value as T;
}

export interface Rows {
  rows?: readonly RecordValue[];
  item?: readonly RecordValue[];
  next_cursor?: string | null;
}
export const rowsOf = (result: Rows) => result.rows ?? result.item ?? [];
