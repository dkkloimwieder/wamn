import * as DropdownMenuPrimitive from "@kobalte/core/dropdown-menu";
import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import { Check } from "lucide-solid";
import type { ComponentProps, ValidComponent } from "solid-js";
import { mergeProps, splitProps } from "solid-js";
import { cn } from "../../lib/utils";

type DropdownMenuProps = DropdownMenuPrimitive.DropdownMenuRootProps;

const DropdownMenu = (props: DropdownMenuProps) => {
  const mergedProps = mergeProps({ gutter: 4 }, props);
  return <DropdownMenuPrimitive.Root data-slot="dropdown-menu" {...mergedProps} />;
};

type DropdownMenuTriggerProps<T extends ValidComponent = "button"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuTriggerProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const DropdownMenuTrigger = <T extends ValidComponent = "button">(
  props: DropdownMenuTriggerProps<T>,
) => {
  const [local, others] = splitProps(props as DropdownMenuTriggerProps, ["class"]);
  return (
    <DropdownMenuPrimitive.Trigger
      class={local.class}
      data-slot="dropdown-menu-trigger"
      {...others}
    />
  );
};

type DropdownMenuContentProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuContentProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const DropdownMenuContent = <T extends ValidComponent = "div">(
  props: DropdownMenuContentProps<T>,
) => {
  const [local, others] = splitProps(props as DropdownMenuContentProps, ["class"]);
  return (
    <DropdownMenuPrimitive.Portal>
      <DropdownMenuPrimitive.Content
        data-slot="dropdown-menu-content"
        class={cn(
          "z-50 z-dropdown-menu-content z-menu-target max-h-(--kb-popper-available-height) min-w-32 origin-(--kb-menu-content-transform-origin) overflow-y-auto overflow-x-hidden outline-none data-closed:overflow-hidden",
          local.class,
        )}
        {...others}
      />
    </DropdownMenuPrimitive.Portal>
  );
};

type DropdownMenuGroupProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuGroupProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const DropdownMenuGroup = <T extends ValidComponent = "div">(props: DropdownMenuGroupProps<T>) => {
  const [local, others] = splitProps(props as DropdownMenuGroupProps, ["class"]);
  return (
    <DropdownMenuPrimitive.Group class={local.class} data-slot="dropdown-menu-group" {...others} />
  );
};

type DropdownMenuLabelProps<T extends ValidComponent = "span"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuGroupLabelProps<T>
> &
  Pick<ComponentProps<T>, "class"> & {
    inset?: boolean;
  };

const DropdownMenuLabel = <T extends ValidComponent = "span">(props: DropdownMenuLabelProps<T>) => {
  const [local, others] = splitProps(props as DropdownMenuLabelProps, ["class", "inset"]);
  return (
    <DropdownMenuPrimitive.GroupLabel
      data-slot="dropdown-menu-label"
      data-inset={local.inset}
      class={cn("z-dropdown-menu-label data-inset:pl-8", local.class)}
      {...others}
    />
  );
};

type DropdownMenuItemProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuItemProps<T>
> &
  Pick<ComponentProps<T>, "class"> & {
    inset?: boolean;
    variant?: "default" | "destructive";
  };

const DropdownMenuItem = <T extends ValidComponent = "div">(rawProps: DropdownMenuItemProps<T>) => {
  const props = mergeProps({ variant: "default" } as DropdownMenuItemProps<T>, rawProps);
  const [local, others] = splitProps(props as DropdownMenuItemProps, ["class", "inset", "variant"]);
  return (
    <DropdownMenuPrimitive.Item
      data-slot="dropdown-menu-item"
      data-inset={local.inset}
      data-variant={local.variant}
      class={cn(
        "group/dropdown-menu-item relative z-dropdown-menu-item flex cursor-default select-none items-center outline-hidden data-disabled:pointer-events-none data-inset:pl-8 data-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0",
        local.class,
      )}
      {...others}
    />
  );
};

type DropdownMenuRadioGroupProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuRadioGroupProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const DropdownMenuRadioGroup = <T extends ValidComponent = "div">(
  props: DropdownMenuRadioGroupProps<T>,
) => {
  const [local, others] = splitProps(props as DropdownMenuRadioGroupProps, ["class"]);
  return (
    <DropdownMenuPrimitive.RadioGroup
      class={local.class}
      data-slot="dropdown-menu-radio-group"
      {...others}
    />
  );
};

type DropdownMenuRadioItemProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuRadioItemProps<T>
> &
  Pick<ComponentProps<T>, "class" | "children">;

const DropdownMenuRadioItem = <T extends ValidComponent = "div">(
  props: DropdownMenuRadioItemProps<T>,
) => {
  const [local, others] = splitProps(props as DropdownMenuRadioItemProps, ["class", "children"]);
  return (
    <DropdownMenuPrimitive.RadioItem
      data-slot="dropdown-menu-radio-item"
      class={cn(
        "relative z-dropdown-menu-radio-item flex cursor-default select-none items-center outline-hidden data-disabled:pointer-events-none data-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0",
        local.class,
      )}
      {...others}
    >
      <span
        class="pointer-events-none z-dropdown-menu-item-indicator"
        data-slot="dropdown-menu-radio-item-indicator"
      >
        <DropdownMenuPrimitive.ItemIndicator>
          <Check />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {local.children}
    </DropdownMenuPrimitive.RadioItem>
  );
};

type DropdownMenuSeparatorProps<T extends ValidComponent = "hr"> = PolymorphicProps<
  T,
  DropdownMenuPrimitive.DropdownMenuSeparatorProps<T>
> &
  Pick<ComponentProps<T>, "class">;

const DropdownMenuSeparator = <T extends ValidComponent = "hr">(
  props: DropdownMenuSeparatorProps<T>,
) => {
  const [local, others] = splitProps(props as DropdownMenuSeparatorProps, ["class"]);
  return (
    <DropdownMenuPrimitive.Separator
      data-slot="dropdown-menu-separator"
      class={cn("z-dropdown-menu-separator", local.class)}
      {...others}
    />
  );
};

export {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
};
