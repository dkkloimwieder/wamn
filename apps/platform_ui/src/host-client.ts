import {
  callOperation,
  reviveOutcome,
  toWire,
  transportFetch,
  type FieldMap,
  type InvokeOptions,
  type JsonValue,
  type OperationBinding,
  type Outcome,
  type ResponseContract,
  type Transport,
} from "@wamn/web-runtime";
import { createClient } from "./generated/wamn_control/client/index.js";
import { createClientConfig } from "./client-config";

interface Schema {
  properties?: Record<string, Schema>;
  items?: Schema;
  oneOf?: Schema[];
}

interface HostOperation {
  operationId: string;
  "x-wamn-contract": ResponseContract;
  parameters?: { name: string; content: { "application/json": { schema: Schema } } }[];
  requestBody?: { content: { "application/json": { schema: Schema } } };
  responses: { 200: { content: { "application/json": { schema: Schema } } } };
}

type HostDocument = { paths: Record<string, { get?: HostOperation; post?: HostOperation }> };
type SdkOperation = (...args: never[]) => Promise<unknown>;

type Camel<S extends string> = S extends `${infer Head}_${infer Tail}` ? `${Head}${Capitalize<Camel<Tail>>}` : S;

type Members<T> = unknown extends T
  ? JsonValue
  : T extends readonly (infer Item)[]
    ? readonly Members<Item>[]
    : T extends object
      ? { [Key in keyof T as Key extends string ? Camel<Key> : Key]: Members<T[Key]> }
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

function fieldMap(properties: Record<string, Schema> = {}): FieldMap {
  return Object.fromEntries(
    Object.entries(properties).map(([name, schema]) => {
      const member = name.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase());
      const nested = schema.properties ?? schema.items?.properties;
      return [name, nested ? { member, fields: fieldMap(nested) } : member];
    }),
  );
}

function binding(template: string, operation: HostOperation, method: string): OperationBinding {
  const request =
    operation.requestBody?.content["application/json"].schema.items?.properties ??
    Object.fromEntries(
      (operation.parameters ?? []).map(({ name, content }) => [name, content["application/json"].schema]),
    );
  const result = operation.responses[200].content["application/json"].schema.items?.oneOf?.find(
    (variant) => variant.properties?.value,
  )?.properties?.value?.properties;
  return {
    route: { operation: operation.operationId, method, template, contract: operation["x-wamn-contract"] },
    request: fieldMap(request),
    result: fieldMap(result),
  };
}

export function hostOperations(document: unknown) {
  const bindings = new Map<string, OperationBinding>();
  for (const [path, { get, post }] of Object.entries((document as HostDocument).paths)) {
    const name = path.split("/").slice(-2).join(".");
    if (get) bindings.set(name, binding(path, get, "GET"));
    if (post) bindings.set(name, binding(path, post, "POST"));
  }

  return function bind<F extends SdkOperation>(name: string, operation: F) {
    const operationBinding = bindings.get(name);
    if (!operationBinding) throw new Error(`Missing host operation ${name}`);
    const { route, request, result } = operationBinding;

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
      client.interceptors.request.use(async (outgoing) => {
        const body = toWire(await outgoing.clone().json(), request);
        return new Request(outgoing, { body: JSON.stringify(body) });
      });
      client.interceptors.response.use(async (reply) => Response.json(reviveOutcome(await reply.json(), result)));
      const send = operation as unknown as (options: {
        client: typeof client;
        body: readonly RequestItem<F>[];
        signal?: AbortSignal;
        responseStyle: "data";
        throwOnError: true;
      }) => Promise<unknown>;
      // The interceptor replaces the wire envelope with the transport outcome.
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
