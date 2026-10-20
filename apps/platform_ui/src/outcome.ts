import type { Outcome } from "@wamn/web-runtime";

export function value<T>(outcome: Outcome<T>): T {
  if (outcome.status === "completed") return outcome.value;
  if (outcome.status === "refused") throw new Error(outcome.text ?? outcome.code ?? "The operation was refused.");
  if (outcome.status === "uncertain") throw new Error(`The result is unknown: ${outcome.reason}`);
  throw new Error(`The operation completed in part: ${JSON.stringify(outcome.failedOutcome)}`);
}
