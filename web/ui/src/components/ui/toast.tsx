import CircleCheck from "lucide-solid/icons/circle-check";
import Info from "lucide-solid/icons/info";
import OctagonX from "lucide-solid/icons/octagon-x";
import TriangleAlert from "lucide-solid/icons/triangle-alert";
import { createSignal, For, type JSX, Show } from "solid-js";
import { Dynamic } from "solid-js/web";

/** What a toast reports, which picks its icon. */
type ToastType = "success" | "info" | "warning" | "error";

interface ToastEntry {
  readonly id: number;
  readonly type: ToastType;
  readonly title: string;
  readonly description?: string | undefined;
}

/** How long a toast shows, in milliseconds. */
const SHOWN_FOR = 4000;

const ICONS = { success: CircleCheck, info: Info, warning: TriangleAlert, error: OctagonX };

const [toasts, setToasts] = createSignal<readonly ToastEntry[]>([]);
let nextId = 0;

function show(type: ToastType, title: string, options?: { readonly description?: string | undefined }) {
  const id = nextId++;
  setToasts((current) => [...current, { id, type, title, description: options?.description }]);
  setTimeout(() => setToasts((current) => current.filter((entry) => entry.id !== id)), SHOWN_FOR);
}

/** Raises a toast, which the mounted `Toaster` shows for four seconds. */
const toast = {
  success: (title: string, options?: { readonly description?: string | undefined }) => show("success", title, options),
  info: (title: string, options?: { readonly description?: string | undefined }) => show("info", title, options),
  warning: (title: string, options?: { readonly description?: string | undefined }) => show("warning", title, options),
  error: (title: string, options?: { readonly description?: string | undefined }) => show("error", title, options),
};

/** The toasts, at the top center of the page. The page mounts it once. */
function Toaster(): JSX.Element {
  return (
    <section aria-label="Notifications" class="toaster group">
      <ol
        data-slot="toaster"
        class="pointer-events-none fixed inset-x-0 top-6 z-[100] mx-auto flex w-[356px] max-w-[calc(100%-2rem)] flex-col gap-2"
      >
        <For each={toasts()}>
          {(entry) => (
            <li
              data-slot="toast"
              data-type={entry.type}
              role="status"
              aria-live="polite"
              class="z-toast pointer-events-auto flex items-center gap-1.5 border bg-popover p-4 font-sans text-[13px] text-popover-foreground shadow-lg"
            >
              <Dynamic component={ICONS[entry.type]} class="size-4 shrink-0" />
              <div class="flex flex-col gap-0.5">
                <div class="font-medium">{entry.title}</div>
                <Show when={entry.description}>{(description) => <div>{description()}</div>}</Show>
              </div>
            </li>
          )}
        </For>
      </ol>
    </section>
  );
}

export { toast, Toaster };
