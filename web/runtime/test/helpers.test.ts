/**
 * The helpers that generated components call: the page state, the draft
 * members, and the cell text.
 */

import { describe, expect, it } from "vitest";

import { ABSENT_CELL, cellText } from "../src/cell.js";
import { clearMember, readMember, writeMember } from "../src/draft.js";
import { appendPage, emptyPage, firstPage, hasNextPage, startRead, stopRead } from "../src/page.js";
import { refusedMember } from "../src/transport.js";

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
    expect(emptyPage<{ id: string }>()).toEqual({ rows: [], cursor: null, busy: false });
  });

  it("states that a request is in flight and that it finished", () => {
    const busy = startRead(emptyPage<{ id: string }>());
    expect(busy.busy).toBe(true);
    expect(hasNextPage({ ...busy, cursor: "c1" })).toBe(false);
    expect(stopRead(busy).busy).toBe(false);
  });

  it("replaces the rows of a bounded list, which carries no cursor", () => {
    const read = firstPage<{ id: string }>([{ id: "a" }]);
    expect(read).toEqual({ rows: [{ id: "a" }], cursor: null, busy: false });
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

describe("the member a refusal names", () => {
  it("reads a declared field member", () => {
    expect(refusedMember({ field: "code" })).toBe("code");
  });

  it("reads the last segment of a schema pointer", () => {
    expect(refusedMember({ data: { pointer: "/0/change/code" } })).toBe("code");
    expect(refusedMember({ pointer: "/0/id" })).toBe("id");
  });

  it("names nothing when the refusal names nothing", () => {
    expect(refusedMember(null)).toBeNull();
    expect(refusedMember({ constraint: "widget_code_key" })).toBeNull();
    expect(refusedMember(["code"])).toBeNull();
  });
});
