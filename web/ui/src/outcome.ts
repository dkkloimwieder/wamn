/**
 * One outcome, shown to the operator as a toast.
 *
 * The page mounts the `Toaster` once. A generated component calls this after
 * it reads an outcome, and it still hands the outcome to its caller. A refusal
 * that names a field still marks that field in place, and the toast does not
 * replace the mark.
 */

import type { Outcome } from "@wamn/web-runtime";
import { toast } from "solid-sonner";

/** Shows one outcome of the screen that `screen` names. */
export function announceOutcome<T>(outcome: Outcome<T>, screen: string): void {
  switch (outcome.status) {
    case "completed":
      toast.success(`${screen}: completed`);
      return;
    case "partiallyCompleted":
      toast.warning(`${screen}: partially completed`);
      return;
    case "refused":
      toast.error(
        `${screen}: refused`,
        outcome.code === null ? {} : { description: outcome.code },
      );
      return;
    case "uncertain":
      toast.warning(`${screen}: uncertain`, { description: outcome.reason });
      return;
  }
}
