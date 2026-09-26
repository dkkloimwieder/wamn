/**
 * The sentence an operator reads for one refusal code.
 *
 * The platform declares a closed set of codes, and ingress states a few more.
 * Each has its own sentence here. An application declares its own business
 * codes, such as `purchase_order_not_open`. When the operation declares text
 * for one, the refusal carries it and the sentence is that text. Otherwise the
 * sentence is the code in words. A refusal that states no code reads as a
 * plain refusal.
 */

const PLATFORM: { readonly [code: string]: string } = {
  unauthenticated: "You are not signed in.",
  "permission-denied": "You do not have permission to do this.",
  "schema-invalid": "A value is not valid.",
  "malformed-json": "The request could not be read.",
  "mapped-payload-too-large": "The request is too large.",
  "route-capacity-exhausted": "The server was busy. Try again.",
  invalid_input: "A value is not valid.",
  not_found: "The record does not exist.",
  concurrency_conflict:
    "Another change saved this record after you opened it. Read it again and retry.",
  idempotency_conflict: "This request was already sent with other values.",
  unique_violation: "Another record already uses this value.",
  foreign_key_violation: "This change breaks a link to another record.",
  check_violation: "A value breaks a rule of this record.",
  exclusion_violation: "This value overlaps another record.",
  retry: "The server was busy. Try again.",
  timeout: "The request took too long. Try again.",
  permission_denied: "You do not have permission to do this.",
  internal_error: "The server could not complete the request.",
};

/** The sentence for one refusal code, or the text its operation declares for it. */
export function refusalSentence(code: string | null, text?: string): string {
  if (text !== undefined && text !== "") {
    return text;
  }
  if (code === null || code === "") {
    return "The request was refused.";
  }
  const platform = PLATFORM[code];
  if (platform !== undefined) {
    return platform;
  }
  const words = code.replace(/[_-]/g, " ");
  return `${words.charAt(0).toUpperCase()}${words.slice(1)}.`;
}
