import type { OutboxCounts } from "@wfe/sync";

/** Refused entries survive a reload. Only unsent work blocks it. */
export function canReload(counts: OutboxCounts): boolean {
  return counts.pending === 0 && counts.sending === 0;
}
