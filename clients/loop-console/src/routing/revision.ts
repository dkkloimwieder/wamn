/**
 * The one seam between the router's opaque revision segment and the reader's
 * numeric one. `parseHash` deliberately never reads the segment — that is what
 * keeps a malformed URL from throwing — and `drafts.get` takes a number, so the
 * conversion belongs here, once, ahead of the read.
 */

/** A canonical non-negative decimal integer, or null — never a coerced number. */
export function parseRevision(segment: string): number | null {
  // Form, not length: route.ts already bounds the segment at MAX_ID_LENGTH.
  // Matched rather than coerced because every alternative spelling `Number`
  // accepts — "017", "+1", "1e3", "0x11", " 17 " — would make two URLs address
  // one revision, and one of them would be the one the author cannot retype.
  if (!/^(?:0|[1-9][0-9]*)$/.test(segment)) {
    return null;
  }
  const revision = Number(segment);
  // Above the safe range the digits and the number stop agreeing.
  return Number.isSafeInteger(revision) ? revision : null;
}
