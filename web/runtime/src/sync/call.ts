import type { OperationCall } from "@wfe/sync";

import { requestHash } from "./hash.js";
import { uuidv7 } from "./ids.js";

/** The create request before the binding supplies its id and send metadata. */
export interface CreateCallOptions<Request extends object> {
  readonly operation: string;
  readonly model: string;
  readonly request: Omit<Request, "id" | "idempotencyKey" | "requestId">;
  readonly append_only: boolean;
}

/** Build one create call and its row id before enqueue. */
export async function createCall<Request extends object>(
  options: CreateCallOptions<Request>,
): Promise<{ call: OperationCall; id: string }> {
  const id = uuidv7();
  const request = { ...options.request, id } as Record<string, unknown>;
  delete request.idempotencyKey;
  delete request.requestId;
  const call: OperationCall = {
    operation: options.operation,
    request,
    request_hash: await requestHash(request),
    expect: { model: options.model, id },
    append_only: options.append_only,
  };
  return { call, id };
}
