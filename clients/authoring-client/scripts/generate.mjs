import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

// The contract's one live source is the Rust authoring model, which prints it.
// There is no checked-in copy of the schema to drift against.
const SCHEMA_SOURCE = "cargo run -p wamn-authoring-model --example print-authoring-surface-schema";
const workspaceRoot = fileURLToPath(new URL("../../../", import.meta.url));
const outputUrl = new URL("../src/generated/authoring.ts", import.meta.url);

const printed = spawnSync(
  "cargo",
  ["run", "--quiet", "-p", "wamn-authoring-model", "--example", "print-authoring-surface-schema"],
  { cwd: workspaceRoot, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
);
if (printed.status !== 0) {
  throw new Error(`${SCHEMA_SOURCE} failed with status ${printed.status ?? "signal"}`);
}
const schemaBytes = printed.stdout;
const schema = JSON.parse(schemaBytes);

const supportedKeywords = new Set([
  "$ref",
  "$schema",
  "additionalProperties",
  "allOf",
  "anyOf",
  // Annotation only in draft-07: it constrains no instance and changes no
  // generated type. An optional field is already optional through `required`.
  "default",
  "definitions",
  "description",
  "enum",
  "format",
  "items",
  "minLength",
  "maximum",
  "minimum",
  "oneOf",
  "properties",
  "required",
  "title",
  "type",
  "x-max-utf8-bytes",
]);
const supportedFormats = new Set(["uint32", "uint64"]);

function assertSupportedSchema(node, path = "$") {
  if (typeof node === "boolean") return;
  if (node === null || typeof node !== "object" || Array.isArray(node)) {
    throw new Error(`${path} must be a JSON Schema object`);
  }
  for (const keyword of Object.keys(node)) {
    if (!supportedKeywords.has(keyword)) {
      throw new Error(`${path} uses unsupported schema keyword ${keyword}`);
    }
  }
  if (node.$schema !== undefined && node.$schema !== "http://json-schema.org/draft-07/schema#") {
    throw new Error(`${path} uses unsupported JSON Schema dialect ${String(node.$schema)}`);
  }
  if (node.format !== undefined && !supportedFormats.has(node.format)) {
    throw new Error(`${path} uses unsupported schema format ${String(node.format)}`);
  }
  for (const keyword of ["anyOf", "oneOf"]) {
    for (const [index, member] of (node[keyword] ?? []).entries()) {
      assertSupportedSchema(member, `${path}.${keyword}[${index}]`);
    }
  }
  if (node.allOf !== undefined) {
    if (
      !Array.isArray(node.allOf) ||
      node.allOf.length !== 1 ||
      Object.keys(node.allOf[0] ?? {}).length !== 1 ||
      typeof node.allOf[0]?.$ref !== "string"
    ) {
      throw new Error(`${path}.allOf must contain exactly one $ref`);
    }
    assertSupportedSchema(node.allOf[0], `${path}.allOf[0]`);
  }
  if (node.items !== undefined) assertSupportedSchema(node.items, `${path}.items`);
  for (const [name, definition] of Object.entries(node.definitions ?? {})) {
    assertSupportedSchema(definition, `${path}.definitions.${name}`);
  }
  for (const [name, property] of Object.entries(node.properties ?? {})) {
    assertSupportedSchema(property, `${path}.properties.${name}`);
  }
}

assertSupportedSchema(schema);

function literal(value) {
  return JSON.stringify(value);
}

function referencedType(ref) {
  const prefix = "#/definitions/";
  if (typeof ref !== "string" || !ref.startsWith(prefix)) {
    throw new Error(`unsupported schema reference: ${String(ref)}`);
  }
  return ref.slice(prefix.length);
}

function schemaType(node) {
  if (node === true) return "unknown";
  if (node === false) return "never";
  if (node.$ref !== undefined) {
    return referencedType(node.$ref);
  }
  if (Array.isArray(node.allOf)) {
    return schemaType(node.allOf[0]);
  }
  if (Array.isArray(node.enum)) {
    return node.enum.map(literal).join(" | ");
  }
  if (Array.isArray(node.oneOf)) {
    return node.oneOf.map((member) => `(${schemaType(member)})`).join(" | ");
  }
  if (Array.isArray(node.anyOf)) {
    return node.anyOf.map((member) => `(${schemaType(member)})`).join(" | ");
  }
  if (Array.isArray(node.type)) {
    return node.type.map((member) => schemaType({ type: member })).join(" | ");
  }

  switch (node.type) {
    case "array":
      return `Array<${schemaType(node.items ?? {})}>`;
    case "boolean":
      return "boolean";
    case "integer":
    case "number":
      return "number";
    case "null":
      return "null";
    case "object": {
      const required = new Set(node.required ?? []);
      const properties = Object.entries(node.properties ?? {}).map(
        ([name, property]) =>
          `  ${literal(name)}${required.has(name) ? "" : "?"}: ${schemaType(property)};`,
      );
      if (node.additionalProperties !== false) {
        properties.push("  [key: string]: unknown;");
      }
      return properties.length === 0 ? "Record<string, unknown>" : `{\n${properties.join("\n")}\n}`;
    }
    case "string":
      return "string";
    case undefined:
      return "unknown";
    default:
      throw new Error(`unsupported schema type: ${String(node.type)}`);
  }
}

const version = schema.definitions?.AuthoringRequest?.properties?.["schema-version"]?.enum?.[0];
if (typeof version !== "string") {
  throw new Error("AuthoringRequest must declare one string schema-version");
}

const digest = createHash("sha256").update(schemaBytes).digest("hex");
const definitions = Object.entries(schema.definitions ?? {})
  .map(([name, definition]) => `export type ${name} = ${schemaType(definition)};`)
  .join("\n\n");

const output = `// @generated by scripts/generate.mjs; DO NOT EDIT.\n// Source: ${SCHEMA_SOURCE} (SHA-256 ${digest})\n\nexport const AUTHORING_SCHEMA_VERSION = ${literal(version)} as const;\n\n${definitions}\n\nexport type AuthoringDocument = ${schemaType(schema)};\nexport type AuthoringCommandKind = AuthoringCommand["kind"];\nexport type AuthoringQueryKind = AuthoringQuery["kind"];\n\n// Runtime validation consumes this exact generated schema, not a handwritten DTO model.\nexport const authoringSchema: unknown = ${JSON.stringify(schema, null, 2)};\n`;

if (process.argv.includes("--check")) {
  let committed;
  try {
    committed = await readFile(outputUrl, "utf8");
  } catch (error) {
    console.error(`generated output is missing: ${fileURLToPath(outputUrl)}`);
    process.exitCode = 1;
    throw error;
  }
  if (committed !== output) {
    console.error(
      `generated output drifted; run: node ${fileURLToPath(import.meta.url)}`,
    );
    process.exitCode = 1;
  }
} else {
  await writeFile(outputUrl, output);
}
