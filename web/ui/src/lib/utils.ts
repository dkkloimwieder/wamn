import { type ClassValue, clsx } from "clsx";

/**
 * One class string from many. A default that a caller overrides sits in the
 * base-layer style class of its component, so the caller's utility class wins
 * without a merge.
 */
export function cn(...inputs: ClassValue[]): string {
  return clsx(inputs);
}
