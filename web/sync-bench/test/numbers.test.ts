import { action, createRoot, createSignal, flush, latest, until } from "solid-js";
import { expect, it, vi } from "vitest";
import { hasNumber } from "@wamn/web-runtime";

it("waits to copy a number until a feed change supplies it", async () => {
  type Row = { id: string; number?: string };
  const copy = vi.fn();
  const scope = createRoot((dispose) => {
    const [row, setRow] = createSignal<Row>({ id: "019fd531-287b-7000-8000-00000000abcd" });
    const copyNumber = action(function* () {
      yield until(() => hasNumber(row(), "number"));
      copy(latest(row).number);
    });
    return { dispose, row, setRow, copyNumber };
  });
  try {
    const copying = scope.copyNumber();
    flush();
    await Promise.resolve();
    expect(copy).not.toHaveBeenCalled();
    scope.setRow({ ...scope.row(), number: "PO-0042" });
    flush();
    await copying;
    expect(copy).toHaveBeenCalledExactlyOnceWith("PO-0042");
  } finally {
    scope.dispose();
  }
});
