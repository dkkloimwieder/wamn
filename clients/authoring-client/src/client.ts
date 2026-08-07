import {
  AUTHORING_SCHEMA_VERSION,
  type AuthoringDocument,
  type AuthoringOutcome,
  type AuthoringRequest,
} from "./generated/authoring.js";
import {
  AuthoringPayloadError,
  parseAuthoringDocument,
  parseAuthoringRequest,
} from "./validate.js";

export type AuthoringRequestDocument = Extract<AuthoringDocument, { document: "request" }>;
export type AuthoringResponseDocument = Extract<AuthoringDocument, { document: "response" }>;

export interface AuthoringTransport {
  execute(request: AuthoringRequestDocument): Promise<unknown>;
}

export class AuthoringNetworkError extends Error {
  override readonly cause: unknown;

  constructor(cause: unknown) {
    super("authoring request failed before receiving an HTTP response");
    this.name = "AuthoringNetworkError";
    this.cause = cause;
  }
}

export class AuthoringHttpError extends Error {
  readonly status: number;

  constructor(status: number) {
    super(`authoring endpoint returned HTTP ${status}`);
    this.name = "AuthoringHttpError";
    this.status = status;
  }
}

export class AuthoringProtocolError extends Error {
  override readonly cause: unknown;

  constructor(message: string, cause?: unknown) {
    super(message);
    this.name = "AuthoringProtocolError";
    this.cause = cause;
  }
}

export class AuthoringClient {
  readonly #transport: AuthoringTransport;

  constructor(transport: AuthoringTransport) {
    this.#transport = transport;
  }

  async execute(request: AuthoringRequest): Promise<AuthoringOutcome> {
    try {
      parseAuthoringRequest(request);
    } catch (error) {
      if (error instanceof AuthoringPayloadError) {
        throw new AuthoringProtocolError("invalid authoring request", error);
      }
      throw error;
    }

    const requestDocument: AuthoringRequestDocument = { document: "request", body: request };
    const payload = await this.#transport.execute(requestDocument);

    let responseDocument: AuthoringResponseDocument;
    try {
      const document = parseAuthoringDocument(payload);
      if (document.document !== "response") {
        throw new AuthoringProtocolError("authoring endpoint returned a request document");
      }
      responseDocument = document;
    } catch (error) {
      if (error instanceof AuthoringProtocolError) throw error;
      if (error instanceof AuthoringPayloadError) {
        throw new AuthoringProtocolError("invalid authoring response", error);
      }
      throw error;
    }

    if (responseDocument.body["schema-version"] !== AUTHORING_SCHEMA_VERSION) {
      throw new AuthoringProtocolError("authoring response has an unsupported schema version");
    }
    if (responseDocument.body["command-id"] !== request["command-id"]) {
      throw new AuthoringProtocolError("authoring response command-id does not match the request");
    }
    if (responseDocument.body.outcome.value.command !== request.command.kind) {
      throw new AuthoringProtocolError("authoring response command does not match the request");
    }
    return responseDocument.body.outcome;
  }
}

export interface FetchResponse {
  readonly ok: boolean;
  readonly status: number;
  json(): Promise<unknown>;
}

export type FetchLike = (
  endpoint: string,
  init: {
    readonly body: string;
    readonly headers: Readonly<Record<string, string>>;
    readonly method: "POST";
  },
) => Promise<FetchResponse>;

export interface FetchTransportOptions {
  readonly endpoint: string;
  readonly fetch: FetchLike;
  readonly headers?: Readonly<Record<string, string>>;
}

export function createFetchTransport(options: FetchTransportOptions): AuthoringTransport {
  return {
    async execute(request) {
      let response: FetchResponse;
      try {
        response = await options.fetch(options.endpoint, {
          body: JSON.stringify(request),
          headers: {
            accept: "application/json",
            "content-type": "application/json",
            ...options.headers,
          },
          method: "POST",
        });
      } catch (error) {
        throw new AuthoringNetworkError(error);
      }

      if (!response.ok) throw new AuthoringHttpError(response.status);
      try {
        return await response.json();
      } catch (error) {
        throw new AuthoringProtocolError("authoring response is not valid JSON", error);
      }
    },
  };
}
