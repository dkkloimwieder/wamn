import type { Reply, Send } from "@wfe/sync";

import { newRequestId } from "../supplied.js";
import type { FieldMap, InvokeOptions, Outcome, Transport } from "../wire.js";

/** One generated binding, and the generated request field map that says where its idempotency key goes. */
export interface SendBinding<Request> {
  readonly invoke: (
    transport: Transport,
    items: readonly Request[],
    options?: InvokeOptions,
  ) => Promise<Outcome<unknown>>;
  readonly fields: FieldMap;
}

/** The transport, generated bindings and session renewal for one outbox. */
export interface SendOptions<Requests extends Record<string, unknown>> {
  readonly transport: Transport;
  readonly bindings: { readonly [Operation in keyof Requests]: SendBinding<Requests[Operation]> };
  readonly renew: () => Promise<boolean>;
}

/** The member path of the `idempotency_key` input, from the generated request field map. */
export function idempotencyKeyPath(fields: FieldMap): readonly string[] | undefined {
  for (const [wire, entry] of Object.entries(fields)) {
    if (typeof entry === "string") {
      if (wire === "idempotency_key") return [entry];
      continue;
    }
    const inner = idempotencyKeyPath(entry.fields);
    if (inner !== undefined) return [entry.member, ...inner];
  }
  return undefined;
}

/** A copy of the request with the value set at the member path. */
function withMember(request: unknown, path: readonly string[], value: string): Record<string, unknown> {
  const [head, ...rest] = path;
  const object = { ...(request as Record<string, unknown>) };
  object[head!] = rest.length === 0 ? value : withMember(object[head!] ?? {}, rest, value);
  return object;
}

/** Send one stored request through its generated binding, renewing the session at most once. */
export function createSend<Requests extends Record<string, unknown>>(options: SendOptions<Requests>): Send {
  const keyPaths = new Map<string, readonly string[]>();
  for (const [operation, binding] of Object.entries(options.bindings) as [string, SendBinding<unknown>][]) {
    const path = idempotencyKeyPath(binding.fields);
    if (path === undefined) throw new Error(`${operation} takes no idempotency key, so the outbox cannot replay it`);
    keyPaths.set(operation, path);
  }
  return async (entry, signal) => {
    if (!Object.hasOwn(options.bindings, entry.operation)) {
      return { ok: false, retriable: false, refusal: { code: "unknown_operation", text: "unknown_operation" } };
    }
    const binding = options.bindings[entry.operation]!;
    const keyed = withMember(entry.request, keyPaths.get(entry.operation)!, entry.request_hash);
    const send = () =>
      binding.invoke(options.transport, [{ ...keyed, requestId: newRequestId() } as Requests[string]], { signal });
    if (signal.aborted) return { ok: false, retriable: true };
    let outcome = await send();
    if (signal.aborted) return { ok: false, retriable: true };
    if (outcome.status === "refused" && outcome.code === "unauthenticated") {
      try {
        if (!(await options.renew()) || signal.aborted) return { ok: false, retriable: true };
      } catch {
        return { ok: false, retriable: true };
      }
      outcome = await send();
    }
    return signal.aborted ? { ok: false, retriable: true } : reply(outcome);
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
