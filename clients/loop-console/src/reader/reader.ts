import type { AuthoringReader } from "./contract";
import { fixtureReader } from "./fixture-reader";

/**
 * The dev toggle. `VITE_READER=http` asks for the live reader; anything else,
 * including unset, is fixtures — so the console runs on fixtures by default and
 * nothing has to be configured to develop against them.
 *
 * STEP 9 SEAM: the HTTP reader over the generated read surface replaces the
 * throw below. Nothing in this repository speaks HTTP to the authoring
 * endpoint yet, and step 9 is blocked on the read legs being mounted, so asking
 * for it now fails loudly rather than silently serving fixtures.
 */
export function selectReader(mode: string | undefined = import.meta.env["VITE_READER"]): AuthoringReader {
  if (mode === "http") {
    throw new Error("VITE_READER=http: the HTTP reader lands in step 9");
  }
  return fixtureReader;
}
