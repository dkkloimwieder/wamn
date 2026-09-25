/**
 * The query-string encoding of a read item against the vectors that the Rust
 * encoder and the router decoder also read.
 */

import { describe, expect, it } from "vitest";

import vectors from "../../../apps/platform/execution/contract/read-query-vectors.json" with { type: "json" };
import { encodeReadQuery } from "../src/readQuery.js";
import type { JsonValue } from "../src/wire.js";

interface Vector {
  readonly item: { readonly [name: string]: JsonValue };
  readonly query: string;
}

const VECTORS = vectors as readonly Vector[];

describe("encodeReadQuery", () => {
  it("writes each shared vector's query", () => {
    expect(VECTORS.length).toBeGreaterThan(0);
    for (const vector of VECTORS) {
      expect(encodeReadQuery(vector.item)).toBe(vector.query);
    }
  });
});
