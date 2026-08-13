import {
  authoringSchema,
  type AuthoringDocument,
  type AuthoringRequest,
  type AuthoringResponse,
} from "./generated/authoring.js";

type JsonSchema = JsonSchemaObject | boolean;

type JsonSchemaObject = {
  readonly $ref?: string;
  readonly $schema?: string;
  readonly additionalProperties?: boolean;
  readonly allOf?: ReadonlyArray<JsonSchema>;
  readonly anyOf?: ReadonlyArray<JsonSchema>;
  readonly default?: unknown;
  readonly definitions?: Readonly<Record<string, JsonSchema>>;
  readonly description?: string;
  readonly enum?: ReadonlyArray<unknown>;
  readonly format?: string;
  readonly items?: JsonSchema;
  readonly maximum?: number;
  readonly minimum?: number;
  readonly oneOf?: ReadonlyArray<JsonSchema>;
  readonly properties?: Readonly<Record<string, JsonSchema>>;
  readonly required?: ReadonlyArray<string>;
  readonly title?: string;
  readonly type?: string | ReadonlyArray<string>;
};

type RootSchema = JsonSchemaObject & {
  readonly definitions: Readonly<Record<string, JsonSchema>>;
};

const rootSchema = authoringSchema as RootSchema;
const supportedKeywords = new Set([
  "$ref",
  "$schema",
  "additionalProperties",
  "allOf",
  "anyOf",
  // Annotation only in draft-07: it constrains no instance, so validation
  // ignores it exactly as it ignores `description`.
  "default",
  "definitions",
  "description",
  "enum",
  "format",
  "items",
  "maximum",
  "minimum",
  "oneOf",
  "properties",
  "required",
  "title",
  "type",
]);
const supportedFormats = new Set(["uint32", "uint64"]);

export function assertSupportedAuthoringSchema(value: unknown, path = "$"): void {
  if (typeof value === "boolean") return;
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${path} must be a JSON Schema object`);
  }
  const schema = value as JsonSchemaObject;
  for (const keyword of Object.keys(schema)) {
    if (!supportedKeywords.has(keyword)) {
      throw new Error(`${path} uses unsupported schema keyword ${keyword}`);
    }
  }
  if (schema.$schema !== undefined && schema.$schema !== "http://json-schema.org/draft-07/schema#") {
    throw new Error(`${path} uses unsupported JSON Schema dialect ${schema.$schema}`);
  }
  if (schema.format !== undefined && !supportedFormats.has(schema.format)) {
    throw new Error(`${path} uses unsupported schema format ${schema.format}`);
  }
  if (schema.allOf !== undefined) {
    const [reference] = schema.allOf;
    if (
      schema.allOf.length !== 1 ||
      reference === null ||
      typeof reference !== "object" ||
      Array.isArray(reference) ||
      Object.keys(reference).length !== 1 ||
      typeof reference.$ref !== "string"
    ) {
      throw new Error(`${path}.allOf must contain exactly one $ref`);
    }
    assertSupportedAuthoringSchema(reference, `${path}.allOf[0]`);
  }
  for (const keyword of ["anyOf", "oneOf"] as const) {
    for (const [index, member] of (schema[keyword] ?? []).entries()) {
      assertSupportedAuthoringSchema(member, `${path}.${keyword}[${index}]`);
    }
  }
  if (schema.items !== undefined) assertSupportedAuthoringSchema(schema.items, `${path}.items`);
  for (const [name, definition] of Object.entries(schema.definitions ?? {})) {
    assertSupportedAuthoringSchema(definition, `${path}.definitions.${name}`);
  }
  for (const [name, property] of Object.entries(schema.properties ?? {})) {
    assertSupportedAuthoringSchema(property, `${path}.properties.${name}`);
  }
}

assertSupportedAuthoringSchema(rootSchema);

export class AuthoringPayloadError extends Error {
  readonly path: string;

  constructor(path: string, detail: string) {
    super(`${path} ${detail}`);
    this.name = "AuthoringPayloadError";
    this.path = path;
  }
}

function fail(path: string, detail: string): never {
  throw new AuthoringPayloadError(path, detail);
}

function validateType(value: unknown, expected: string, path: string): void {
  switch (expected) {
    case "array":
      if (!Array.isArray(value)) fail(path, "must be an array");
      return;
    case "integer":
      // Normative, not a stopgap: wamn-ftfc.21 settled the wire domain for every
      // authoring integer at [0, 2^53-1], so `Number.isSafeInteger` is exactly the
      // contract. A value outside it is refused, never rounded into a wrong identity.
      if (typeof value !== "number" || !Number.isSafeInteger(value)) {
        fail(path, "must be an exactly representable integer");
      }
      return;
    case "null":
      if (value !== null) fail(path, "must be null");
      return;
    case "number":
      if (typeof value !== "number" || !Number.isFinite(value)) fail(path, "must be a number");
      return;
    case "object":
      if (value === null || typeof value !== "object" || Array.isArray(value)) {
        fail(path, "must be an object");
      }
      return;
    default:
      if (typeof value !== expected) fail(path, `must be ${expected}`);
  }
}

function validateFormat(value: unknown, format: string, path: string): void {
  if (value === null) return;
  // The final uint64 contract (wamn-ftfc.21): the schema carries `maximum`
  // 9007199254740991 on every uint64 site and the Rust boundary refuses anything
  // above it, so this check agrees with the server rather than papering over it.
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    fail(path, `must be an exactly representable ${format}`);
  }
  if (format === "uint32" && (value < 0 || value > 4_294_967_295)) {
    fail(path, "must be between 0 and 4294967295 for uint32");
  }
  if (format === "uint64" && value < 0) {
    fail(path, "must be nonnegative for uint64");
  }
}

function validate(value: unknown, schema: JsonSchema, path: string): void {
  if (schema === true) return;
  if (schema === false) fail(path, "is forbidden by the schema");

  if (schema.$ref !== undefined) {
    const prefix = "#/definitions/";
    if (!schema.$ref.startsWith(prefix)) {
      throw new Error(`unsupported generated schema reference ${schema.$ref}`);
    }
    const name = schema.$ref.slice(prefix.length);
    const definition = rootSchema.definitions[name];
    if (definition === undefined) {
      throw new Error(`missing generated schema definition ${name}`);
    }
    validate(value, definition, path);
    return;
  }

  if (schema.allOf !== undefined) {
    validate(value, schema.allOf[0]!, path);
  }

  if (schema.oneOf !== undefined) {
    let matches = 0;
    for (const alternative of schema.oneOf) {
      try {
        validate(value, alternative, path);
        matches += 1;
      } catch (error) {
        if (!(error instanceof AuthoringPayloadError)) throw error;
      }
    }
    if (matches !== 1) fail(path, "must match exactly one schema variant");
    return;
  }

  if (schema.anyOf !== undefined) {
    for (const alternative of schema.anyOf) {
      try {
        validate(value, alternative, path);
        return;
      } catch (error) {
        if (!(error instanceof AuthoringPayloadError)) throw error;
      }
    }
    fail(path, "must match a schema variant");
  }

  if (schema.enum !== undefined && !schema.enum.some((candidate) => Object.is(candidate, value))) {
    fail(path, `must be one of ${schema.enum.map(String).join(", ")}`);
  }

  if (schema.type !== undefined) {
    const expectedTypes = Array.isArray(schema.type) ? schema.type : [schema.type];
    let matches = false;
    for (const expected of expectedTypes) {
      try {
        validateType(value, expected, path);
        matches = true;
        break;
      } catch (error) {
        if (!(error instanceof AuthoringPayloadError)) throw error;
      }
    }
    if (!matches) fail(path, `must have type ${expectedTypes.join(" or ")}`);
  }

  if (schema.format !== undefined) validateFormat(value, schema.format, path);

  if (typeof value === "number" && schema.minimum !== undefined && value < schema.minimum) {
    fail(path, `must be at least ${schema.minimum}`);
  }
  if (typeof value === "number" && schema.maximum !== undefined && value > schema.maximum) {
    fail(path, `must be at most ${schema.maximum}`);
  }

  if (Array.isArray(value) && schema.items !== undefined) {
    value.forEach((item, index) => validate(item, schema.items!, `${path}[${index}]`));
  }

  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    const record = value as Record<string, unknown>;
    for (const required of schema.required ?? []) {
      if (!Object.hasOwn(record, required)) fail(path, `is missing required field ${required}`);
    }
    if (schema.additionalProperties === false) {
      for (const name of Object.keys(record)) {
        if (schema.properties?.[name] === undefined) fail(`${path}.${name}`, "is unknown");
      }
    }
    for (const [name, propertySchema] of Object.entries(schema.properties ?? {})) {
      if (Object.hasOwn(record, name)) validate(record[name], propertySchema, `${path}.${name}`);
    }
  }
}

function parseDefinition<T>(value: unknown, name: string): T {
  const definition = rootSchema.definitions[name];
  if (definition === undefined) throw new Error(`missing generated schema definition ${name}`);
  validate(value, definition, "$body");
  return value as T;
}

export function parseAuthoringDocument(value: unknown): AuthoringDocument {
  validate(value, rootSchema, "$document");
  return value as AuthoringDocument;
}

export function parseAuthoringRequest(value: unknown): AuthoringRequest {
  return parseDefinition<AuthoringRequest>(value, "AuthoringRequest");
}

export function parseAuthoringResponse(value: unknown): AuthoringResponse {
  return parseDefinition<AuthoringResponse>(value, "AuthoringResponse");
}
