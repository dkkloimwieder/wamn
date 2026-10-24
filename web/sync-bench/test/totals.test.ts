import { describe, expect, it } from "vitest";
import { exactSum, roundHalfAwayFromZero } from "../src/totals";

describe("exactSum", () => {
  it("sums decimal text without float error", () => {
    expect(exactSum(["0.1", "0.2"])).toBe("0.3");
    expect(exactSum(["1.005", 2, "-0.5"])).toBe("2.505");
  });

  it("sums int32 values exactly at 100,000 rows", () => {
    expect(exactSum(Array.from({ length: 100_000 }, () => 2_147_483_647))).toBe("214748364700000");
  });

  it("returns zero for no values", () => {
    expect(exactSum([])).toBe("0");
  });
});

describe("roundHalfAwayFromZero", () => {
  it("rounds ties away from zero", () => {
    expect(roundHalfAwayFromZero("2.5", 0)).toBe("3");
    expect(roundHalfAwayFromZero("-2.5", 0)).toBe("-3");
    expect(roundHalfAwayFromZero("1.005", 2)).toBe("1.01");
    expect(roundHalfAwayFromZero("1.004", 2)).toBe("1.00");
  });

  it("pads to the requested places", () => {
    expect(roundHalfAwayFromZero(7, 2)).toBe("7.00");
  });

  it("refuses text that is not a decimal", () => {
    expect(() => roundHalfAwayFromZero("1e21", 0)).toThrow("not a decimal");
  });
});
