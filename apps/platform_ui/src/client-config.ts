import { encodeReadQuery, type JsonValue } from "@wamn/web-runtime";
import type { CreateClientConfig } from "./generated/wamn_control/client/index.js";

export const createClientConfig: CreateClientConfig = (config) => ({
  ...config,
  querySerializer: (query) =>
    encodeReadQuery(
      Object.fromEntries(
        Object.entries(query as Record<string, JsonValue | undefined>).filter(([, value]) => value !== undefined),
      ) as Record<string, JsonValue>,
    ),
});
