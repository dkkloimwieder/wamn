import { createSignal } from "solid-js";

/** Derived from reads, per spec §1.4. Step 9 wires it to the real reader. */
export type ReadStatus = "never-contacted" | "ok" | "fail";

const [readStatus, setReadStatus] = createSignal<ReadStatus>("never-contacted");

export { readStatus, setReadStatus };

/** The word shown beside the dot — state is never conveyed by color alone. */
export function readStatusWord(status: ReadStatus): string {
  switch (status) {
    case "never-contacted":
      return "never contacted";
    case "ok":
      return "last read succeeded";
    case "fail":
      return "last read failed";
  }
}
