/**
 * The values a row fills into a form travel in the query of the form address.
 */

import { describe, expect, it } from "vitest";

import { fillPath, filledValues } from "../src/index.js";

describe("the filled values of a form", () => {
  it("write one query value for each member, and read them back", () => {
    const values = { value: { purchaseOrderId: "a b", line: { locationId: "c/d" } } };
    const path = fillPath("receipts/new", values);
    expect(path).toBe("receipts/new?value.purchaseOrderId=a+b&value.line.locationId=c%2Fd");
    const search = Object.fromEntries(new URLSearchParams(path.split("?")[1]));
    expect(filledValues(search)).toEqual(values);
  });

  it("leave the path alone when a row fills nothing", () => {
    expect(fillPath("receipts/new", {})).toBe("receipts/new");
    expect(filledValues({})).toEqual({});
  });
});
