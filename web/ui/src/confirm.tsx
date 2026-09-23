/**
 * One action that the operator confirms first.
 *
 * A generated delete names this and states its words. The alert dialog does
 * not close on an outside click, because the action cannot be undone. The
 * question renders as a paragraph and not a heading, because the page that
 * places a component owns every heading.
 */

import { createSignal, type JSX } from "solid-js";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "./components/ui/alert-dialog";
import { Button } from "./components/ui/button";

export interface ConfirmActionProps {
  /** The text of the button that asks. */
  readonly trigger: string;
  /** The question the dialog asks. */
  readonly question: string;
  /** The text of the button that confirms. */
  readonly confirm: string;
  /** The text of the button that cancels. */
  readonly cancel: string;
  /** Called once the operator confirms. */
  readonly onConfirm: () => void;
}

export function ConfirmAction(props: ConfirmActionProps): JSX.Element {
  const [open, setOpen] = createSignal(false);
  return (
    <AlertDialog open={open()} onOpenChange={setOpen}>
      <AlertDialogTrigger as={Button} variant="destructive">
        {props.trigger}
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle as="p">{props.question}</AlertDialogTitle>
        </AlertDialogHeader>
        <AlertDialogFooter>
          {/* Kobalte names a close button Dismiss unless it is told otherwise. */}
          <AlertDialogCancel aria-label={props.cancel}>{props.cancel}</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            onClick={() => {
              setOpen(false);
              props.onConfirm();
            }}
          >
            {props.confirm}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
