import { Loader2Icon, type LucideProps } from "lucide-solid";
import { splitProps } from "solid-js";

import { cn } from "../../lib/utils";

// The icon's own props, so an optional attribute keeps the type the icon
// declares under exactOptionalPropertyTypes.
type SpinnerProps = LucideProps;

const Spinner = (props: SpinnerProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <Loader2Icon
      data-slot="spinner"
      role="status"
      aria-label="Loading"
      class={cn("size-4 animate-spin", local.class)}
      {...others}
    />
  );
};

export { Spinner };
