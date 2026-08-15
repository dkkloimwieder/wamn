import { describe, expect, it } from "vitest";

import { nodeRunTone, runTone } from "./status";

describe("status tones", () => {
  it("maps every run status the wire can carry", () => {
    expect(runTone("queued")).toBe("neutral");
    expect(runTone("dispatched")).toBe("neutral");
    expect(runTone("running")).toBe("neutral");
    expect(runTone("succeeded")).toBe("ok");
    expect(runTone("failed")).toBe("fail");
    expect(runTone("effect-uncertain")).toBe("uncertain");
  });

  it("maps every node run status", () => {
    expect(nodeRunTone("started")).toBe("neutral");
    expect(nodeRunTone("success")).toBe("ok");
    expect(nodeRunTone("error")).toBe("fail");
  });

  it("is unknown-safe: an unheard-of word is neutral, never a claimed state", () => {
    // the durable vocabularies the wire drops, which a widened projection could
    // start sending back at any time
    expect(runTone("completed")).toBe("neutral");
    expect(runTone("infrastructure-failure")).toBe("neutral");
    expect(nodeRunTone("")).toBe("neutral");
  });
});
