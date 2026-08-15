import {
  AUTHORING_SCHEMA_VERSION,
  type AuthoringDocument,
  type AuthoringOutcome,
  type AuthoringQueryOutcome,
  type AuthoringQueryRequest,
  type AuthoringQueryResponse,
  type AuthoringRequest,
  type AuthoringResponse,
} from "./generated/authoring.js";
import {
  AuthoringPayloadError,
  parseAuthoringDocument,
  parseAuthoringQueryRequest,
  parseAuthoringQueryResponse,
  parseAuthoringRequest,
  parseAuthoringResponse,
} from "./validate.js";

export type AuthoringCommandRequestDocument = {
  readonly document: "request";
  readonly body: AuthoringRequest;
};
export type AuthoringQueryRequestDocument = {
  readonly document: "request";
  readonly body: AuthoringQueryRequest;
};

/// The two methods deliberately stay separate: query code has no command
/// execution method to call accidentally and command retry identity never
/// appears in a query request.
export interface AuthoringTransport {
  executeCommand(request: AuthoringCommandRequestDocument): Promise<unknown>;
  executeQuery(request: AuthoringQueryRequestDocument): Promise<unknown>;
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

function responseBody(payload: unknown): unknown {
  try {
    const document: AuthoringDocument = parseAuthoringDocument(payload);
    if (document.document !== "response") {
      throw new AuthoringProtocolError("authoring endpoint returned a request document");
    }
    return document.body;
  } catch (error) {
    if (error instanceof AuthoringProtocolError) throw error;
    if (error instanceof AuthoringPayloadError) {
      throw new AuthoringProtocolError("invalid authoring response", error);
    }
    throw error;
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
        throw new AuthoringProtocolError("invalid authoring command request", error);
      }
      throw error;
    }
    const payload = await this.#transport.executeCommand({ document: "request", body: request });
    let response: AuthoringResponse;
    try {
      response = parseAuthoringResponse(responseBody(payload));
    } catch (error) {
      if (error instanceof AuthoringProtocolError) throw error;
      if (error instanceof AuthoringPayloadError) {
        throw new AuthoringProtocolError("invalid authoring command response", error);
      }
      throw error;
    }
    if (response["schema-version"] !== AUTHORING_SCHEMA_VERSION) {
      throw new AuthoringProtocolError("authoring response has an unsupported schema version");
    }
    if (response["command-id"] !== request["command-id"]) {
      throw new AuthoringProtocolError("authoring response command-id does not match the request");
    }
    if (response.outcome.value.command !== request.command.kind) {
      throw new AuthoringProtocolError("authoring response command does not match the request");
    }
    return response.outcome;
  }

  async query(request: AuthoringQueryRequest): Promise<AuthoringQueryOutcome> {
    try {
      parseAuthoringQueryRequest(request);
    } catch (error) {
      if (error instanceof AuthoringPayloadError) {
        throw new AuthoringProtocolError("invalid authoring query request", error);
      }
      throw error;
    }
    const payload = await this.#transport.executeQuery({ document: "request", body: request });
    let response: AuthoringQueryResponse;
    try {
      response = parseAuthoringQueryResponse(responseBody(payload));
    } catch (error) {
      if (error instanceof AuthoringProtocolError) throw error;
      if (error instanceof AuthoringPayloadError) {
        throw new AuthoringProtocolError("invalid authoring query response", error);
      }
      throw error;
    }
    if (response["schema-version"] !== AUTHORING_SCHEMA_VERSION) {
      throw new AuthoringProtocolError("authoring query response has an unsupported schema version");
    }
    if (response["query-id"] !== request["query-id"]) {
      throw new AuthoringProtocolError("authoring query response query-id does not match the request");
    }
    if (response.outcome.value.query !== request.query.kind) {
      throw new AuthoringProtocolError("authoring query response operation does not match the request");
    }
    return response.outcome;
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
  async function post(document: AuthoringCommandRequestDocument | AuthoringQueryRequestDocument) {
    let response: FetchResponse;
    try {
      response = await options.fetch(options.endpoint, {
        body: JSON.stringify(document),
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
  }

  return {
    executeCommand(request) {
      return post(request);
    },
    executeQuery(request) {
      return post(request);
    },
  };
}
