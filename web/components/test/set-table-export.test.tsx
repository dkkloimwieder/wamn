/**
 * The CSV export of a SetTable (wamn-vfvx.5).
 *
 * It saves the rows the filters and the search keep, in the order the table
 * shows them, with the visible columns in their order and each cell's shown
 * text. Group rows and the totals are not exported. A QueryTable offers it
 * only over a fully read set.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TableColumn } from "@wamn/ui";

import { button, theButton } from "./dom.js";
import { queryTable, setTable, settled } from "./tables.js";

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly note: string | null;
  readonly region: string;
  readonly maker: string | null;
  readonly secret: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "note", label: "note", type: "text" },
  { field: "region", label: "region", type: "text" },
  { field: "maker", label: "maker", type: "uuid", role: "reference" },
  { field: "secret", label: "secret", type: "text" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 3, note: "plain", region: "east", maker: "m1", secret: "s" },
  { id: "r1", code: "b", qty: 1, note: "one, two", region: "west", maker: null, secret: "s" },
  { id: "r2", code: "c", qty: 2, note: 'say "hi"', region: "east", maker: "m2", secret: "s" },
  { id: "r3", code: "d", qty: 5, note: "two\nlines", region: "west", maker: "m1", secret: "s" },
  { id: "r4", code: "e", qty: 4, note: null, region: "east", maker: "m2", secret: "s" },
];

const HEADER = "code,qty,note,region,maker";

/** The CSV line of each row, as the export writes it. */
const LINE: Record<string, string> = {
  a: "a,3,plain,east,m1",
  b: 'b,1,"one, two",west,',
  c: 'c,2,"say ""hi""",east,m2',
  d: 'd,5,"two\nlines",west,m1',
  e: "e,4,,east,m2",
};

let saved: { name: string; blob: Blob } | null = null;
let blob: Blob | null = null;

beforeEach(() => {
  saved = null;
  URL.createObjectURL = (value: Blob | MediaSource) => {
    blob = value as Blob;
    return "blob:export";
  };
  URL.revokeObjectURL = () => {};
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    saved = { name: this.download, blob: blob! };
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const table = (groupedFields?: readonly (keyof Row & string)[]) =>
  setTable({ columns: COLUMNS, rows: ROWS, hiddenFields: ["secret"], groupedFields });

/** Export, and return the saved bytes as text, with the byte order mark kept. */
async function exported(): Promise<string> {
  fireEvent.click(theButton("export CSV"));
  expect(saved).not.toBeNull();
  const bytes = new Uint8Array(await saved!.blob.arrayBuffer());
  return new TextDecoder("utf-8", { ignoreBOM: true }).decode(bytes);
}

/** The text of an export of the header and the lines of these codes. */
const csv = (codes: string) => `\uFEFF${[HEADER, ...codes.split("").map((code) => LINE[code]!)].join("\r\n")}\r\n`;

describe("the CSV export", () => {
  it("quotes by RFC 4180, ends lines in CRLF, starts with a BOM, and names the file by the table", async () => {
    table();
    expect(await exported()).toBe(csv("abcde"));
    expect(saved!.name).toMatch(/^rows-\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z\.csv$/);
    expect(saved!.blob.type).toBe("text/csv;charset=utf-8");
  });

  it("does not export a hidden column, and a reference exports its id", async () => {
    table();
    const text = await exported();
    expect(text).not.toContain("secret");
    expect(text.split("\r\n")[1]).toBe("a,3,plain,east,m1");
  });

  it("exports only the rows that the filters and the search keep", async () => {
    table();
    fireEvent.input(screen.getByLabelText("search"), { target: { value: "east" } });
    fireEvent.click(theButton("filter qty"));
    fireEvent.input(screen.getByLabelText("min"), { target: { value: "3" } });
    expect(await exported()).toBe(csv("ae"));
  });

  it("keeps the table sort", async () => {
    table();
    fireEvent.click(theButton("qty"));
    expect(await exported()).toBe(csv("bcaed"));
    fireEvent.click(theButton("qty"));
    expect(await exported()).toBe(csv("deacb"));
  });

  it("exports the data rows in the order of their groups, without the group rows or the totals", async () => {
    table(["region"]);
    fireEvent.click(theButton("expand all region"));
    const lines = (await exported()).split("\r\n");
    // Grouping moves the grouped column first, as the table shows it.
    expect(lines[0]).toBe("\uFEFFregion,code,qty,note,maker");
    expect(lines.slice(1, -1)).toEqual([
      "east,a,3,plain,m1",
      'east,c,2,"say ""hi""",m2',
      "east,e,4,,m2",
      'west,b,1,"one, two",',
      'west,d,5,"two\nlines",m1',
    ]);
  });

  it("is not offered on a set that is not fully read, and comes back when it is", async () => {
    const [more, setMore] = createSignal(true);
    await queryTable({ name: "lots", columns: COLUMNS, rows: () => ROWS, more, hiddenFields: ["secret"] });
    expect(button("export CSV")).toBeNull();
    setMore(false);
    fireEvent.click(theButton("refresh"));
    await settled();
    expect(await exported()).toBe(csv("abcde"));
    expect(saved!.name).toMatch(/^lots-/);
  });
});
