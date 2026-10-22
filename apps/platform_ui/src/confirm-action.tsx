import { createSignal, Show } from "solid-js";
import { AlertDialog } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";

export function ConfirmAction(props: { label: string; description: string; run: () => Promise<void> }) {
  const [open, setOpen] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const submit = async () => {
    if (pending()) return;
    setPending(true);
    setTrouble(undefined);
    try {
      await props.run();
      setOpen(false);
    } catch (error) {
      setTrouble(String(error));
    } finally {
      setPending(false);
    }
  };
  return (
    <AlertDialog
      open={open()}
      onOpenChange={(next) => {
        if (!pending()) {
          setOpen(next);
          setTrouble(undefined);
        }
      }}
    >
      <AlertDialog.Trigger as={Button}>{props.label}</AlertDialog.Trigger>
      <AlertDialog.Portal>
        <AlertDialog.Overlay />
        <AlertDialog.Content>
          <AlertDialog.Title>{props.label}?</AlertDialog.Title>
          <AlertDialog.Description>{props.description}</AlertDialog.Description>
          <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
          <AlertDialog.Cancel disabled={pending()}>Cancel</AlertDialog.Cancel>
          <AlertDialog.Action disabled={pending()} onClick={() => void submit()}>
            {props.label}
          </AlertDialog.Action>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog>
  );
}
