import type { Reply, Send } from "@wfe/sync";

import { newRequestId } from "../supplied.js";
import type { Outcome, Transport } from "../wire.js";

/** The transport, generated bindings and session renewal for one outbox. */
export interface SendOptions<Requests extends Record<string, unknown>> {
  readonly transport: Transport;
  readonly bindings: {
    readonly [Operation in keyof Requests]: (
      transport: Transport,
      items: readonly Requests[Operation][],
    ) => Promise<Outcome<unknown>>;
  };
  readonly renew: () => Promise<boolean>;
}

/** Send one stored request through its generated binding, renewing the session at most once. */
export function createSend<Requests extends Record<string, unknown>>(options: SendOptions<Requests>): Send {
  return async (entry, signal) => {
    if (!Object.hasOwn(options.bindings, entry.operation)) {
      return { ok: false, retriable: false, refusal: { code: "unknown_operation", text: "unknown_operation" } };
    }
    const binding = options.bindings[entry.operation]!;
    const send = () =>
      binding(options.transport, [
        {
          ...(entry.request as object),
          idempotencyKey: entry.request_hash,
          requestId: newRequestId(),
        } as Requests[string],
      ]);
    if (signal.aborted) return { ok: false, retriable: true };
    let outcome = await send();
    if (outcome.status === "refused" && outcome.code === "unauthenticated") {
      try {
        if (!(await options.renew()) || signal.aborted) return { ok: false, retriable: true };
      } catch {
        return { ok: false, retriable: true };
      }
      outcome = await send();
    }
    return reply(outcome);
  };
}

function reply(outcome: Outcome<unknown>): Reply {
  switch (outcome.status) {
    case "completed":
      return { ok: true };
    case "uncertain":
      return { ok: false, retriable: true };
    case "partiallyCompleted":
      return { ok: false, retriable: false, refusal: { code: "partial", text: "partial" } };
    case "refused": {
      if (["unauthenticated", "retry", "timeout"].includes(outcome.code ?? "")) {
        return { ok: false, retriable: true };
      }
      const code = outcome.code ?? "refused";
      const text = outcome.text ?? (outcome.detail === null ? code : JSON.stringify(outcome.detail));
      return { ok: false, retriable: false, refusal: { code, text } };
    }
  }
}
