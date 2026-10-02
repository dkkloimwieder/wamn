/**
 * The sentence of one refused Control write.
 *
 * It is the text of the contract. A write that stopped part way through the
 * environments of the org (`application_write_incomplete`) also names the
 * environment where it stopped and the environments that completed.
 */

import { type JsonValue, type Outcome, refusalSentence } from "@wamn/web-runtime";

type Refused = Extract<Outcome<unknown>, { status: "refused" }>;

/** The members of a detail object, or none. */
function members(value: JsonValue | undefined): { readonly [key: string]: JsonValue } {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return {};
  }
  return value as { readonly [key: string]: JsonValue };
}

export function controlRefusal(outcome: Refused): string {
  const sentence = refusalSentence(outcome.code, outcome.text);
  const outer = members(outcome.detail);
  const detail = members(outer["detail"] ?? outcome.detail);
  const environment = detail["environment"];
  if (typeof environment !== "string") {
    return sentence;
  }
  const completed = Array.isArray(detail["completed"]) ? detail["completed"].map(String) : [];
  return `${sentence} Stopped at ${environment}. Completed: ${completed.length > 0 ? completed.join(", ") : "none"}.`;
}
