/**
 * The tooltip: the Kobalte tooltip with the Zaidan look.
 *
 * Kobalte opens, places and dismisses the content. It reports the side it
 * settled on, and the content and its arrow carry that side as `data-side`,
 * which the slide-in animation and the arrow offset read.
 */

import type { PolymorphicProps } from "@kobalte/core/polymorphic";
import * as TooltipPrimitive from "@kobalte/core/tooltip";
import type { ComponentProps, ValidComponent } from "solid-js";
import { createContext, createSignal, mergeProps, splitProps, useContext } from "solid-js";

import { cn } from "../../lib/utils";

const SideContext = createContext<() => string>(() => "top");

/** The side of a Kobalte placement, for example `right` of `right-start`. */
const sideOf = (placement: string) => placement.split("-")[0] ?? placement;

const Tooltip = (props: TooltipPrimitive.TooltipRootProps) => {
  const merged = mergeProps(
    {
      placement: "top",
      gutter: 4,
      overflowPadding: 5,
      arrowPadding: 5,
      hideWhenDetached: true,
      openDelay: 600,
      closeDelay: 0,
    } as const,
    props,
  );
  const [side, setSide] = createSignal(sideOf(merged.placement));
  return (
    <SideContext.Provider value={side}>
      <TooltipPrimitive.Root
        {...merged}
        onCurrentPlacementChange={(placement) => {
          setSide(sideOf(placement));
          props.onCurrentPlacementChange?.(placement);
        }}
      />
    </SideContext.Provider>
  );
};

const TooltipTrigger = TooltipPrimitive.Trigger;

type TooltipContentProps<T extends ValidComponent = "div"> = PolymorphicProps<
  T,
  TooltipPrimitive.TooltipContentProps<T>
> &
  Partial<Pick<ComponentProps<T>, "class" | "children">>;

const TooltipContent = <T extends ValidComponent = "div">(props: TooltipContentProps<T>) => {
  const side = useContext(SideContext);
  const [local, others] = splitProps(props as TooltipContentProps, ["class", "children"]);
  return (
    <TooltipPrimitive.Portal>
      <TooltipPrimitive.Content
        data-slot="tooltip-content"
        data-side={side()}
        class={cn(
          "z-50 z-tooltip-content w-fit max-w-xs origin-(--kb-tooltip-content-transform-origin) bg-foreground text-background",
          local.class,
        )}
        {...others}
      >
        {local.children}
        <TooltipPrimitive.Arrow
          size={10}
          data-side={side()}
          class="z-50 z-tooltip-arrow size-2.5 rotate-45 bg-foreground fill-foreground data-[side=bottom]:translate-y-[calc(50%+2px)] data-[side=left]:translate-x-[calc(-50%-2px)] data-[side=right]:translate-x-[calc(50%+2px)] data-[side=top]:translate-y-[calc(-50%-2px)] [&_svg]:hidden"
        />
      </TooltipPrimitive.Content>
    </TooltipPrimitive.Portal>
  );
};

export type { TooltipContentProps };
export { Tooltip, TooltipContent, TooltipTrigger };
