import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import * as PopoverPrimitive from "@kobalte/core/popover";
import type { ComponentProps, ValidComponent } from "solid-js";
import { mergeProps, splitProps } from "solid-js";
import { cn } from "../../lib/utils";

type PopoverProps = PopoverPrimitive.PopoverRootProps;

const Popover = (props: PopoverProps) => {
  const mergedProps = mergeProps({ gutter: 4, placement: "bottom" } as const, props);
  return <PopoverPrimitive.Root data-slot="popover" {...mergedProps} />;
};

type PopoverTriggerProps<T extends ValidComponent = "button"> = PolymorphicProps<
  T,
  PopoverPrimitive.PopoverTriggerProps<T>
>;

const PopoverTrigger = <T extends ValidComponent = "button">(props: PopoverTriggerProps<T>) => {
  return <PopoverPrimitive.Trigger data-slot="popover-trigger" {...props} />;
};

type PopoverContentProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  PopoverPrimitive.PopoverContentProps<T>
> &
  Pick<ComponentProps<T>, "class" | "children">;

const PopoverContent = <T extends ValidComponent = "div">(props: PopoverContentProps<T>) => {
  const [local, others] = splitProps(props as PopoverContentProps, ["class", "children"]);

  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        data-slot="popover-content"
        class={cn(
          "z-50 z-popover-content w-72 origin-(--kb-popover-content-transform-origin) outline-hidden",
          local.class,
        )}
        {...others}
      >
        {local.children}
      </PopoverPrimitive.Content>
    </PopoverPrimitive.Portal>
  );
};

type PopoverTitleProps<T extends ValidComponent = "h2"> = PolymorphicProps<
  T,
  PopoverPrimitive.PopoverTitleProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const PopoverTitle = <T extends ValidComponent = "h2">(props: PopoverTitleProps<T>) => {
  const [local, others] = splitProps(props as PopoverTitleProps, ["class"]);
  return (
    <PopoverPrimitive.Title
      data-slot="popover-title"
      class={cn("z-font-heading z-popover-title", local.class)}
      {...others}
    />
  );
};

export { Popover, PopoverContent, PopoverTitle, PopoverTrigger };
