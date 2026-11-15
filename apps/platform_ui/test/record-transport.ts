import { createTransport } from "@wamn/web-runtime";

/** Record the real wire response for generated-client field checks. */
export function recordTransport(baseUrl: string, credential: string | undefined) {
  let body: Array<{ value: Record<string, unknown> }> = [];
  const transport = createTransport({
    baseUrl,
    credential,
    fetch: async (...args) => {
      const response = await fetch(...args);
      body = await response.clone().json();
      return response;
    },
  });
  return { transport, value: () => body[0]!.value };
}
