import type { ShellResult } from "@wfe/sync";

/** The startup result, without another storage request from the screen. */
export function shellStatus(result?: ShellResult): string {
  if (!result) return "Worker: starting. Persisted: pending. Quota: pending.";
  return `Worker: ${result.worker ? "registered" : "unavailable"}. Persisted: ${result.persisted ? "yes" : "no"}. Storage ${result.persisted ? "protected" : "not protected"}. Quota: ${result.quota.quota} bytes. Used: ${result.quota.usage} bytes.`;
}
