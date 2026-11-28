import {
  callOperation,
  transportFetch,
  type InvokeOptions,
  type JsonValue,
  type OperationBinding,
  type Outcome,
  type ResponseContract,
  type Transport,
} from "@wamn/web-runtime";
import { createClient } from "./generated/wamn_control/client/index.js";
import { createClientConfig } from "./client-config";

interface HostOperation {
  operationId: string;
  "x-wamn-contract": ResponseContract;
}

type HostDocument = { paths: Record<string, { get?: HostOperation; post?: HostOperation; put?: HostOperation }> };
type SdkOperation = (...args: never[]) => Promise<unknown>;

type Members<T> = unknown extends T
  ? JsonValue
  : T extends readonly (infer Item)[]
    ? readonly Members<Item>[]
    : T extends object
      ? { [Key in keyof T]: Members<T[Key]> }
      : T;

type RequestItem<F extends SdkOperation> =
  NonNullable<Parameters<F>[0]> extends { body: readonly (infer Item)[] }
    ? Members<Item>
    : NonNullable<Parameters<F>[0]> extends { query?: infer Query }
      ? [NonNullable<Query>] extends [never]
        ? Record<string, never>
        : Members<NonNullable<Query>>
      : Record<string, never>;

type ValueIn<Row> = Row extends { value: infer Value } ? Value : never;
type ResponseData<F extends SdkOperation> = Extract<Awaited<ReturnType<F>>, { data: readonly unknown[] }>["data"];
type Result<F extends SdkOperation> = Members<ValueIn<ResponseData<F>[number]>>;

export type OperationResult<F extends (transport: Transport, items: never) => Promise<Outcome<unknown>>> = Extract<
  Awaited<ReturnType<F>>,
  { status: "completed" }
>["value"];

function binding(template: string, operation: HostOperation, method: string): OperationBinding {
  return {
    route: { operation: operation.operationId, method, template, contract: operation["x-wamn-contract"] },
  };
}

export function hostOperations(document: unknown) {
  const bindings = new Map<string, OperationBinding>();
  for (const [path, { get, post, put }] of Object.entries((document as HostDocument).paths)) {
    const name = path.split("/").slice(-2).join(".");
    if (get) bindings.set(name, binding(path, get, "GET"));
    if (post) bindings.set(name, binding(path, post, "POST"));
    if (put) bindings.set(name, binding(path, put, "PUT"));
  }

  return function bind<F extends SdkOperation>(name: string, operation: F) {
    const operationBinding = bindings.get(name);
    if (!operationBinding) throw new Error(`Missing host operation ${name}`);
    const { route } = operationBinding;

    const invoke = async (
      transport: Transport,
      items: readonly RequestItem<F>[],
      options?: InvokeOptions,
    ): Promise<Outcome<Result<F>>> => {
      if (route.method === "GET") return callOperation(transport, operationBinding, items, options);

      const client = createClient(
        createClientConfig({
          // The transport supplies the authenticated origin.
          baseUrl: "https://wamn.invalid",
          fetch: transportFetch(transport, route),
        }),
      );
      const send = operation as unknown as (options: {
        client: typeof client;
        body: readonly RequestItem<F>[];
        signal?: AbortSignal;
        responseStyle: "data";
        throwOnError: true;
      }) => Promise<unknown>;
      // The transport supplies the operation outcome as the response body.
      return (await send({
        client,
        body: items,
        signal: options?.signal,
        responseStyle: "data",
        throwOnError: true,
      })) as Outcome<Result<F>>;
    };

    return Object.assign(invoke, { route });
  };
}
