import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { failingRun } from "../reader/fixtures";
import { SectionLabel } from "./section-label";

afterEach(cleanup);

describe("SectionLabel", () => {
  it("is a section heading wearing the label role", () => {
    const { container } = render(() => <SectionLabel label="execution" />);
    const heading = container.querySelector("h2");
    expect(heading).toHaveTextContent("execution");
    // the 12px uppercase tracked dim role is the shared `.label`, not a restatement
    expect(heading).toHaveClass("label");
    // §1.4: the hairline running out of the heading is what makes this a divider
    // and not a plain heading, so it is part of the contract, not decoration
    expect(heading?.nextElementSibling).toHaveClass("section-rule");
  });

  it("holds the trailing slot beside the heading, out of its accessible name", () => {
    const count = `${failingRun.factCount.returned} facts, all`;
    const { container } = render(() => (
      <SectionLabel label="execution" trailing={<span>{count}</span>} />
    ));
    expect(container).toHaveTextContent(count);
    expect(container.querySelector("h2")?.textContent).toBe("execution");
    // §2.2 draws the count at the far end of the rule, not against the label
    expect(container.querySelector(".section-rule")?.nextElementSibling).toHaveTextContent(count);
    cleanup();

    // §2.6's refresh control sits in the same slot, and stays a real button
    const refreshed = render(() => (
      <SectionLabel label="execution" trailing={<button type="button">refresh execution</button>} />
    ));
    expect(refreshed.container.querySelector("button")).toHaveTextContent("refresh execution");
    expect(refreshed.container.querySelector("h2")?.textContent).toBe("execution");
  });
});
