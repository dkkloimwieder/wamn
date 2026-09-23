import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** One class string from many, where a later Tailwind class wins a conflict. */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
