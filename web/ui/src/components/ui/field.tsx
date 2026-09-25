import { cva, type VariantProps } from "class-variance-authority";
import type { ComponentProps, JSX } from "solid-js";
import {
  createMemo,
  For,
  mergeProps,
  children as resolveChildren,
  Show,
  splitProps,
} from "solid-js";

import { cn } from "../../lib/utils";
import { Label } from "./label";

type FieldSetProps = ComponentProps<"fieldset"> & {
  class?: string | undefined;
};

const FieldSet = (props: FieldSetProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <fieldset
      data-slot="field-set"
      class={cn("z-field-set flex flex-col", local.class)}
      {...others}
    />
  );
};

type FieldLegendProps = ComponentProps<"legend"> & {
  class?: string | undefined;
  variant?: "legend" | "label";
};

const FieldLegend = (props: FieldLegendProps) => {
  const mergedProps = mergeProps({ variant: "legend" } as const, props);
  const [local, others] = splitProps(mergedProps, ["class", "variant"]);

  return (
    <legend
      data-slot="field-legend"
      data-variant={local.variant}
      class={cn("z-field-legend", local.class)}
      {...others}
    />
  );
};

type FieldGroupProps = ComponentProps<"div"> & {
  class?: string | undefined;
};

const FieldGroup = (props: FieldGroupProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <div
      data-slot="field-group"
      class={cn(
        "z-field-group group/field-group @container/field-group flex w-full flex-col",
        local.class,
      )}
      {...others}
    />
  );
};

const fieldVariants = cva("z-field group/field flex w-full", {
  variants: {
    orientation: {
      vertical: "z-field-orientation-vertical flex-col *:w-full [&>.sr-only]:w-auto",
      horizontal:
        "z-field-orientation-horizontal flex-row items-center has-[>[data-slot=field-content]]:items-start *:data-[slot=field-label]:flex-auto has-[>[data-slot=field-content]]:[&>[role=checkbox],[role=radio]]:mt-px",
      responsive:
        "z-field-orientation-responsive flex-col *:w-full @md/field-group:flex-row @md/field-group:items-center @md/field-group:*:w-auto @md/field-group:has-[>[data-slot=field-content]]:items-start @md/field-group:*:data-[slot=field-label]:flex-auto [&>.sr-only]:w-auto @md/field-group:has-[>[data-slot=field-content]]:[&>[role=checkbox],[role=radio]]:mt-px",
    },
  },
  defaultVariants: {
    orientation: "vertical",
  },
});

type FieldProps = ComponentProps<"div"> &
  VariantProps<typeof fieldVariants> & {
    class?: string | undefined;
  };

const Field = (props: FieldProps) => {
  const mergedProps = mergeProps({ orientation: "vertical" } as const, props);
  const [local, others] = splitProps(mergedProps, ["class", "orientation"]);

  return (
    // biome-ignore lint/a11y/useSemanticElements: role="group" is intentional per shadcn design for accessibility
    <div
      role="group"
      data-slot="field"
      data-orientation={local.orientation}
      class={cn(fieldVariants({ orientation: local.orientation }), local.class)}
      {...others}
    />
  );
};

type FieldLabelProps = ComponentProps<typeof Label> & {
  class?: string | undefined;
};

const FieldLabel = (props: FieldLabelProps) => {
  const [local, others] = splitProps(props, ["class"]);
  return (
    <Label
      data-slot="field-label"
      class={cn(
        "z-field-label group/field-label peer/field-label flex w-fit",
        "has-[>[data-slot=field]]:w-full has-[>[data-slot=field]]:flex-col",
        local.class,
      )}
      {...others}
    />
  );
};

type FieldErrorProps = ComponentProps<"div"> & {
  class?: string | undefined;
  children?: JSX.Element;
  errors?: Array<{ message?: string } | undefined>;
};

const FieldError = (props: FieldErrorProps) => {
  const [local, others] = splitProps(props, ["class", "children", "errors"]);
  const resolvedChildren = resolveChildren(() => local.children);

  const content = createMemo(() => {
    const childContent = resolvedChildren();

    if (childContent) {
      return childContent;
    }

    if (!local.errors?.length) {
      return null;
    }

    const uniqueErrors = [
      ...new Map(local.errors.map((error) => [error?.message, error])).values(),
    ];

    if (uniqueErrors?.length === 1) {
      return uniqueErrors[0]?.message;
    }

    return (
      <ul class="ml-4 flex list-disc flex-col gap-1">
        <For each={uniqueErrors}>
          {(error) => (
            <Show when={error?.message}>
              <li>{error?.message}</li>
            </Show>
          )}
        </For>
      </ul>
    );
  });

  return (
    <Show when={content()}>
      {(resolvedContent) => (
        <div
          role="alert"
          data-slot="field-error"
          class={cn("z-field-error font-normal", local.class)}
          {...others}
        >
          {resolvedContent()}
        </div>
      )}
    </Show>
  );
};

export {
  Field,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
};
