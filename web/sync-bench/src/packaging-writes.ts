import { recordWrite, type OperationCall, type Outbox, type OutboxEntry } from "@wfe/sync";
import { createCall, requestHash } from "@wamn/web-runtime";
import { PACKAGING_CREATE_ROUTE, type PackagingCreateRequest } from "@wamn/wms-client/packaging.js";
import type { packagingCollection } from "./packaging-feed";

export type PackagingDraft = Omit<PackagingCreateRequest, "id" | "idempotencyKey" | "requestId">;

/** The application route carries its audience in the first path segment. */
export function packagingAddress(path: string): { aud: string; application: string; environment: string } {
  const aud = decodeURIComponent(path.split("/")[1] ?? "");
  const match = /^urn:wamn:project-env:([^:]+):([^:]+):([^:]+):([^:]+)$/.exec(aud);
  if (!match) throw new Error("Open /urn:wamn:project-env:<org>:<project>:<env>:<instance>/live.html.");
  return { aud, application: match[2]!, environment: match[3]! };
}

/** Create through the optimistic store, and keep the same id when correcting a refused request. */
export function packagingWrites(c: ReturnType<typeof packagingCollection>, o: Outbox) {
  const record = recordWrite(c, o);

  function save(call: OperationCall): Promise<void> {
    // The watch sees the committed entry. The write itself still waits for the feed.
    return new Promise((resolve, reject) => {
      let stop = () => {};
      stop = o.watch(() => {
        if (o.entries().some((entry) => entry.request_hash === call.request_hash)) {
          stop();
          resolve();
        }
      });
      void record.write(call, call.request as PackagingDraft & { id: string }).catch((error: unknown) => {
        // After recording, refusal and OutboxDiscarded revert the overlay. The panel owns the refusal.
        stop();
        reject(error);
      });
    });
  }

  return {
    rows: record.rows,
    async create(request: PackagingDraft): Promise<void> {
      const { call } = await createCall<PackagingCreateRequest>({
        operation: PACKAGING_CREATE_ROUTE.operation,
        model: "packaging",
        request,
        append_only: true,
      });
      await save(call);
    },
    async resubmit(entry: OutboxEntry, edited: string): Promise<void> {
      const value: unknown = JSON.parse(edited);
      if (value === null || typeof value !== "object" || Array.isArray(value)) {
        throw new Error("The request must be a JSON object.");
      }
      const request: Record<string, unknown> = { ...value, id: entry.expect.id };
      delete request.idempotencyKey;
      delete request.requestId;
      const hash = await requestHash(request);
      if (hash === entry.request_hash) throw new Error("Edit the request before resubmitting it.");
      await save({
        operation: entry.operation,
        request,
        request_hash: hash,
        expect: entry.expect,
        append_only: entry.append_only,
      });
      await o.discard(entry.seq);
      void o.replay().catch(() => undefined);
    },
  };
}
