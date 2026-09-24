/**
 * The helpers that generated components call: the page state, the draft
 * members, the repeated group bounds, and the cell text.
 */

import { describe, expect, it } from "vitest";

import { ABSENT_CELL, cellText } from "../src/cell.js";
import { clearMember, readMember, writeControl, writeMember } from "../src/draft.js";
import { canAdd, canRemove } from "../src/group.js";
import { appendPage, emptyPage, firstPage, hasNextPage, startRead, stopRead } from "../src/page.js";
import { checkedMember, refusalMarks, refusedMember } from "../src/transport.js";

describe("the page state", () => {
  it("appends the next page and keeps the rows already shown", () => {
    const first = firstPage<{ id: string }>([{ id: "a" }], "c1");
    expect(first.rows).toEqual([{ id: "a" }]);
    expect(hasNextPage(first)).toBe(true);
    const second = appendPage(first, [{ id: "b" }], null);
    expect(second.rows).toEqual([{ id: "a" }, { id: "b" }]);
    expect(hasNextPage(second)).toBe(false);
  });

  it("clears the cursor with the rows, because it names a place in the old list", () => {
    expect(emptyPage<{ id: string }>()).toEqual({ rows: [], cursor: null, busy: false, refusal: null });
  });

  it("states that a request is in flight and that it finished", () => {
    const busy = startRead(emptyPage<{ id: string }>());
    expect(busy.busy).toBe(true);
    expect(hasNextPage({ ...busy, cursor: "c1" })).toBe(false);
    expect(stopRead(busy).busy).toBe(false);
  });

  it("replaces the rows of a bounded list, which carries no cursor", () => {
    const read = firstPage<{ id: string }>([{ id: "a" }]);
    expect(read).toEqual({ rows: [{ id: "a" }], cursor: null, busy: false, refusal: null });
  });
});

describe("a draft member", () => {
  it("reads and writes a nested member without touching the rest", () => {
    const draft = { requestId: "r1", change: { code: "priority", note: "keep" } };
    expect(readMember(draft, ["change", "code"])).toBe("priority");
    const written = writeMember(draft, ["change", "code"], "standard");
    expect(written).toEqual({ requestId: "r1", change: { code: "standard", note: "keep" } });
    expect(draft.change.code).toBe("priority");
  });

  it("creates a parent that the draft does not carry yet", () => {
    expect(writeMember({}, ["change", "code"], "priority")).toEqual({
      change: { code: "priority" },
    });
  });

  it("reads nothing through a member that is absent", () => {
    expect(readMember({ change: null }, ["change", "code"])).toBeUndefined();
    expect(readMember({}, ["change"])).toBeUndefined();
  });

  it("clears an omittable member", () => {
    const draft = { change: { code: "priority", note: "keep" } };
    expect(clearMember(draft, ["change", "note"])).toEqual({ change: { code: "priority" } });
  });

  it("sends no member for an empty page control, and no parent it leaves empty", () => {
    const both = writeControl({}, ["filter", "code"], ["A-1"]);
    const two = writeControl(both, ["filter", "note"], ["x"]);
    expect(two).toEqual({ filter: { code: ["A-1"], note: ["x"] } });
    // One control emptied: that member goes, and the other stays.
    const one = writeControl(two, ["filter", "note"], []);
    expect(one).toEqual({ filter: { code: ["A-1"] } });
    // Every control emptied: the read carries no filter member at all.
    expect(writeControl(one, ["filter", "code"], [])).toEqual({});
    expect(writeControl({ limit: 5 }, ["limit"], "")).toEqual({});
    expect(writeControl({}, ["filter", "code"], [])).toEqual({});
  });
});

describe("the cell text", () => {
  it("states an absent value and a null as nothing", () => {
    expect(cellText(undefined, "text")).toBe(ABSENT_CELL);
    expect(cellText(null, "uuid")).toBe(ABSENT_CELL);
  });

  it("keeps every digit of a decimal and of a 64-bit integer", () => {
    expect(cellText("12.3400", "numeric")).toBe("12.3400");
    expect(cellText("9007199254740993", "int64")).toBe("9007199254740993");
  });

  it("reads a timestamp as a date and a time in UTC", () => {
    expect(cellText("2026-09-21T12:34:56.000000Z", "timestamptz")).toBe("2026-09-21 12:34:56");
    expect(cellText("not a timestamp", "timestamptz")).toBe("not a timestamp");
  });

  it("states a boolean, a count of bytes, and a json value", () => {
    expect(cellText(true, "boolean")).toBe("true");
    expect(cellText(false, "boolean")).toBe("false");
    expect(cellText([1, 2, 3], "bytes")).toBe("3 bytes");
    expect(cellText({ fooBar: 1 }, "json")).toBe('{"fooBar":1}');
  });
});

describe("the path a refusal names", () => {
  it("reads a declared field member", () => {
    expect(refusedMember({ field: "code" })).toBe("code");
    expect(refusedMember({ field: "value.line[].quantity" })).toBe("value.line[].quantity");
  });

  it("reads a schema pointer as the declared path it names", () => {
    expect(refusedMember({ data: { pointer: "/0/change/code" } })).toBe("change.code");
    expect(refusedMember({ pointer: "/0/id" })).toBe("id");
    expect(refusedMember({ data: { pointer: "/0/value/line/2/quantity" } })).toBe(
      "value.line[2].quantity",
    );
  });

  it("descends into the detail that the error carries beside its code", () => {
    expect(refusedMember({ detail: { field: "value.line[].quantity" } })).toBe(
      "value.line[].quantity",
    );
  });

  it("names nothing when the refusal names nothing", () => {
    expect(refusedMember(null)).toBeNull();
    expect(refusedMember({ constraint: "widget_code_key" })).toBeNull();
    expect(refusedMember(["code"])).toBeNull();
    expect(refusedMember({ detail: { constraint: "widget_code_key" } })).toBeNull();
  });
});

describe("the control a refused path marks", () => {
  it("marks the control whose declared path it names", () => {
    expect(refusalMarks("change.code", "change.code")).toBe(true);
    expect(refusalMarks("change.code", "change.note")).toBe(false);
    expect(refusalMarks(null, "change.code")).toBe(false);
  });

  it("marks one line when the refusal names its index", () => {
    expect(refusalMarks("value.line[2].quantity", "value.line[].quantity", 2)).toBe(true);
    expect(refusalMarks("value.line[2].quantity", "value.line[].quantity", 1)).toBe(false);
  });

  it("marks every line when the refusal names no index", () => {
    expect(refusalMarks("value.line[].quantity", "value.line[].quantity", 0)).toBe(true);
    expect(refusalMarks("value.line[].quantity", "value.line[].quantity", 7)).toBe(true);
    expect(refusalMarks("value.line[].quantity", "value.line[].location_id", 0)).toBe(false);
  });
});

describe("the bounds of a repeated group", () => {
  it("stops adding at the declared maximum", () => {
    expect(canAdd([], 2)).toBe(true);
    expect(canAdd([1, 2], 2)).toBe(false);
    // A group with no declared maximum never stops.
    expect(canAdd([1, 2, 3], null)).toBe(true);
  });

  it("stops removing at the declared minimum", () => {
    expect(canRemove([1, 2], 1)).toBe(true);
    expect(canRemove([1], 1)).toBe(false);
    // There is nothing to remove.
    expect(canRemove([], 0)).toBe(false);
  });
});

describe("the member a checked input refuses", () => {
  const FIELDS = {
    request_id: "requestId",
    value: {
      member: "value",
      fields: {
        maker_id: "makerId",
        "line[]": {
          member: "line",
          fields: { quantity: "quantity" },
        },
      },
    },
  };

  it("states the contract's spelling, with the element it refused", () => {
    expect(checkedMember(["value", "makerId"], FIELDS)).toBe("value.maker_id");
    expect(checkedMember(["value", "line", 2, "quantity"], FIELDS)).toBe(
      "value.line[2].quantity",
    );
  });

  it("states nothing for a path no field map holds", () => {
    expect(checkedMember(undefined, FIELDS)).toBe(null);
    expect(checkedMember([], FIELDS)).toBe(null);
    expect(checkedMember(["value", "unknown"], FIELDS)).toBe(null);
  });

  it("marks the control that names the same declared path", () => {
    const member = checkedMember(["value", "line", 2, "quantity"], FIELDS);
    expect(refusalMarks(member, "value.line[].quantity", 2)).toBe(true);
    expect(refusalMarks(member, "value.line[].quantity", 1)).toBe(false);
  });
});
