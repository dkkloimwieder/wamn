/**
 * The behavior of the conversion pair, which `wamn-lajk.2` emitted and left
 * type validated only.
 *
 * The emitter decides every member name and writes a field map beside each
 * operation. The pair renames the keys a map declares and leaves every other
 * key alone, so the inside of a `json` value survives a round trip.
 */

import { describe, expect, it } from "vitest";

import type { FieldMap } from "../src/wire.js";
import { fromWire, reviveOutcome, toWire } from "../src/wire.js";

const REQUEST: FieldMap = {
  request_id: "requestId",
  selector: "selector",
  change: {
    member: "change",
    fields: { unit_price: "unitPrice", line_1: "line_1" },
  },
};

const PAGE: FieldMap = {
  item: { member: "item", fields: { edit_version: "editVersion" } },
  next_cursor: "nextCursor",
};

describe("the conversion pair", () => {
  it("renames a declared member in both directions", () => {
    const wire = { request_id: "r1", change: { unit_price: "1.50", line_1: "a" } };
    const members = fromWire(wire, REQUEST);
    {
      const observed = members;
      expect(observed).toHaveProperty(["requestId"], "r1");
      expect(observed).toHaveProperty(["change", "unitPrice"], "1.50");
      expect(observed).toHaveProperty(["change", "line_1"], "a");
    }
    expect(toWire(members, REQUEST)).toEqual(wire);
  });

  it("leaves the inside of a json value alone", () => {
    const wire = { selector: { fooBar: 1, nested: { alsoHere: true } } };
    const members = fromWire(wire, REQUEST);
    expect(members).toEqual(wire);
    expect(toWire(members, REQUEST)).toEqual(wire);
  });

  it("keeps a key that no map declares", () => {
    const wire = { request_id: "r1", added_later: 2 };
    {
      const observed = fromWire(wire, REQUEST);
      expect(observed).toHaveProperty(["requestId"], "r1");
      expect(observed).toHaveProperty(["added_later"], 2);
    }
  });

  it("renames each row of a declared collection", () => {
    const wire = { item: [{ edit_version: "4" }, { edit_version: "5" }], next_cursor: null };
    {
      const observed = fromWire(wire, PAGE);
      expect(observed).toHaveProperty(["item", "length"], 2);
      expect(observed).toHaveProperty(["item", "0", "editVersion"], "4");
      expect(observed).toHaveProperty(["item", "1", "editVersion"], "5");
      expect(observed).toHaveProperty(["nextCursor"], null);
    }
  });

  it("renames a completed value and leaves a refusal in wire spelling", () => {
    const completed = reviveOutcome<unknown>(
      { status: "completed", value: { item: [{ edit_version: "4" }], next_cursor: null } },
      PAGE,
    );
    {
      const observed = completed;
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "item", "length"], 1);
      expect(observed).toHaveProperty(["value", "item", "0", "editVersion"], "4");
      expect(observed).toHaveProperty(["value", "nextCursor"], null);
    }

    const refused = reviveOutcome<unknown>(
      { status: "refused", code: "concurrency_conflict", detail: { expected_row_version: "4" } },
      PAGE,
    );
    {
      const observed = refused;
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "concurrency_conflict");
      expect(observed).toHaveProperty(["detail", "expected_row_version"], "4");
    }
  });
});
