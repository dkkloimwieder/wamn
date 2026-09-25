import type {
  ComboboxContentProps as ComboboxPrimitiveContentProps,
  ComboboxInputProps as ComboboxPrimitiveInputProps,
  ComboboxItemProps as ComboboxPrimitiveItemProps,
  ComboboxRootProps,
} from "@kobalte/core/combobox";
import * as ComboboxPrimitive from "@kobalte/core/combobox";
import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import { Check, ChevronsUpDown, X } from "lucide-solid";
import type { ComponentProps, JSX, ValidComponent } from "solid-js";
import { mergeProps, Show, splitProps } from "solid-js";
import { cn } from "../../lib/utils";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
} from "./input-group";

// ============================================================================
// Combobox Root
// ============================================================================

type ComboboxProps<O, OptGroup = never, T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  ComboboxRootProps<O, OptGroup, T>
> &
  Pick<ComponentProps<T>, "class" | "children">;

const Combobox = <O, OptGroup = never, T extends ValidComponent = "div">(
  props: ComboboxProps<O, OptGroup, T>,
) => {
  const mergedProps = mergeProps(
    {
      sameWidth: true,
      gutter: 8,
      placement: "bottom",
      defaultFilter: "contains",
      triggerMode: "input",
    } as ComboboxProps<O>,
    props,
  );
  return <ComboboxPrimitive.Root {...mergedProps} />;
};

// ============================================================================
// Combobox Control
// ============================================================================

// ============================================================================
// Combobox Chips
// ============================================================================

// ============================================================================
// Combobox Input
// ============================================================================

type ComboboxInputProps<T extends ValidComponent = "input"> = PolymorphicProps<
  T,
  ComboboxPrimitiveInputProps<T>
> &
  Pick<ComponentProps<"input">, "class" | "placeholder" | "disabled" | "id" | "name"> & {
    showTrigger?: boolean;
    showClear?: boolean;
    children?: JSX.Element;
  };

const ComboboxInput = <T extends ValidComponent = "input">(rawProps: ComboboxInputProps<T>) => {
  const context = ComboboxPrimitive.useComboboxContext();
  const isDisabled = () => context.isDisabled() || local.disabled;
  const props = mergeProps({ showTrigger: true, showClear: false }, rawProps);
  const [local, others] = splitProps(props as ComboboxInputProps, [
    "class",
    "showTrigger",
    "showClear",
    "children",
    "disabled",
  ]);

  return (
    <ComboboxPrimitive.Control
      as={InputGroup}
      class={cn("z-combobox-input w-auto", local.class)}
      data-slot="combobox-control"
    >
      {(state) => (
        <>
          {local.children}
          <ComboboxPrimitive.Input
            as={InputGroupInput}
            disabled={isDisabled()}
            data-slot="combobox-input"
            {...others}
          />
          <InputGroupAddon align="inline-end">
            <Show when={local.showTrigger}>
              <ComboboxPrimitive.Trigger
                as={InputGroupButton}
                size="icon-xs"
                variant="ghost"
                data-slot="combobox-trigger"
                class="group-has-data-[slot=combobox-clear]/input-group:hidden data-pressed:bg-transparent"
                disabled={isDisabled()}
              >
                <ComboboxPrimitive.Icon
                  as={ChevronsUpDown}
                  class="pointer-events-none z-combobox-trigger-icon"
                />
              </ComboboxPrimitive.Trigger>
            </Show>
            <Show when={local.showClear && state.selectedOptions().length > 0}>
              <InputGroupButton
                variant="ghost"
                size="icon-xs"
                data-slot="combobox-clear"
                class="z-combobox-clear"
                disabled={isDisabled()}
                aria-label="Clear selection"
                onClick={() => {
                  if (!isDisabled()) state.clear();
                }}
              >
                <X class="pointer-events-none z-combobox-clear-icon" />
              </InputGroupButton>
            </Show>
          </InputGroupAddon>
        </>
      )}
    </ComboboxPrimitive.Control>
  );
};

// ============================================================================
// Combobox Trigger (for popup-style combobox)
// ============================================================================

// ============================================================================
// Combobox Content
// ============================================================================

type ComboboxContentProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  ComboboxPrimitiveContentProps<T>
> &
  Pick<ComponentProps<T>, "class"> & {
    /**
     * Content below the list, inside the popup. Platform addition: Kobalte
     * gives no end-of-list event, so a next page control sits here.
     */
    footer?: JSX.Element;
  };

const ComboboxContent = <T extends ValidComponent = "div">(props: ComboboxContentProps<T>) => {
  const context = ComboboxPrimitive.useComboboxContext();
  const [local, others] = splitProps(props as ComboboxContentProps, [
    "class",
    "onCloseAutoFocus",
    "footer",
  ]);
  return (
    <ComboboxPrimitive.Portal>
      <ComboboxPrimitive.Content
        class={cn(
          "relative isolate z-50 z-combobox-content z-menu-target max-h-(--kb-popper-available-height) min-w-32 origin-(--kb-combobox-content-transform-origin) overflow-y-auto overflow-x-hidden",
          local.class,
        )}
        data-slot="combobox-content"
        onCloseAutoFocus={(event) => {
          local.onCloseAutoFocus?.(event);
          // Restoring input focus after the exit animation would reopen a focus-triggered popup.
          if (context.triggerMode() === "focus") event.preventDefault();
        }}
        {...others}
      >
        <ComboboxPrimitive.Listbox class="z-combobox-listbox m-0 p-1" />
        {local.footer}
      </ComboboxPrimitive.Content>
    </ComboboxPrimitive.Portal>
  );
};

// ============================================================================
// Combobox Section (Group)
// ============================================================================

// ============================================================================
// Combobox Section Label
// ============================================================================

// ============================================================================
// Combobox Item
// ============================================================================

type ComboboxItemProps<T extends ValidComponent = "li"> = PolymorphicProps<
  T,
  ComboboxPrimitiveItemProps<T>
> &
  Pick<ComponentProps<T>, "class"> & {
    children?: JSX.Element;
  };

const ComboboxItem = <T extends ValidComponent = "li">(props: ComboboxItemProps<T>) => {
  const [local, others] = splitProps(props as ComboboxItemProps, ["class", "children"]);
  return (
    <ComboboxPrimitive.Item
      class={cn(
        "relative z-combobox-item z-select-item flex w-full cursor-default select-none items-center outline-hidden data-disabled:pointer-events-none data-disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0",
        local.class,
      )}
      data-slot="combobox-item"
      {...others}
    >
      <ComboboxPrimitive.ItemLabel class="z-combobox-item-label z-select-item-text shrink-0 whitespace-nowrap">
        {local.children}
      </ComboboxPrimitive.ItemLabel>
      <ComboboxPrimitive.ItemIndicator
        as="span"
        class="z-combobox-item-indicator z-select-item-indicator"
      >
        <Check class="pointer-events-none z-combobox-item-indicator-icon z-select-item-indicator-icon" />
      </ComboboxPrimitive.ItemIndicator>
    </ComboboxPrimitive.Item>
  );
};

// ============================================================================
// Combobox Empty
// ============================================================================

// ============================================================================
// Combobox Separator
// ============================================================================

export {
  Combobox,
  ComboboxContent,
  ComboboxInput,
  ComboboxItem,
};
