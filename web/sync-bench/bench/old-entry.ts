import { outbox } from "@wfe/sync";
import { createCall, createSend, createTransport, outboxDatabase } from "@wamn/web-runtime";
import * as r1 from "@release/r1";
import * as r2 from "@release/r2";

/** Record with R1, reopen with R2, and compare with an ordinary invalid request. */
export async function run(locationId: string) {
  const transport = createTransport({ baseUrl: `${location.origin}/wamn` });
  const operation = r1.PACKAGING_CREATE_ROUTE.operation;
  const request: Omit<r1.PackagingCreateRequest, "id" | "idempotencyKey" | "requestId"> = {
    packagingCode: 123,
    locationId,
    type: "tote",
    status: "available",
  };
  const { call } = await createCall<r1.PackagingCreateRequest>({
    operation,
    model: "packaging",
    request,
    append_only: true,
  });
  // Exercise the old generated request builder before saving its entry.
  let recorded: unknown;
  await r1.create(
    {
      invoke: async (wire) => {
        recorded = wire.items[0];
        return { status: "completed", value: {} };
      },
    },
    [
      {
        ...(call.request as r1.PackagingCreateRequest),
        idempotencyKey: call.request_hash,
        requestId: crypto.randomUUID(),
      },
    ],
  );
  const database = outboxDatabase("wms", "release-test", crypto.randomUUID());
  const old = outbox({ database, send: async () => ({ ok: false, retriable: true }), probe: async () => false });
  await old.enqueue(call);
  old.close();
  const send = createSend<Record<string, r2.PackagingCreateRequest>>({
    transport,
    bindings: { [operation]: { invoke: r2.create, fields: r2.PACKAGING_CREATE_REQUEST_FIELDS } },
    renew: async () => false,
  });
  const current = outbox({ database, send, probe: () => transport.probe() });
  try {
    const deadline = Date.now() + 30_000;
    while (current.entries()[0]?.state !== "refused") {
      if (Date.now() > deadline) throw new Error("R2 did not refuse the old entry");
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    const entry = current.entries()[0]!;
    const ordinary = await r2.create(transport, [
      {
        ...(call.request as r2.PackagingCreateRequest),
        requestId: crypto.randomUUID(),
        idempotencyKey: crypto.randomUUID(),
      },
    ]);
    const id = crypto.randomUUID();
    const accepted = await r2.create(transport, [
      {
        id,
        packagingCode: "R2",
        locationId,
        type: "tote",
        status: "available",
        requestId: crypto.randomUUID(),
        idempotencyKey: crypto.randomUUID(),
      },
    ]);
    return { recorded, entry, ordinary, accepted, ids: [id] };
  } finally {
    current.close();
  }
}
