/**
 * The bounds of one repeated input group.
 *
 * The contract declares how many elements a submission may carry, and the
 * emitted schema states the same two numbers. These two answer the other
 * half: whether the operator can add or remove one right now. The rule is the
 * same for every form, so it lives here and not in generated code.
 */

/** Whether one more element stays inside the declared maximum. */
export function canAdd(elements: readonly unknown[], maximum: number | null): boolean {
  return maximum === null || elements.length < maximum;
}

/** Whether one fewer element stays inside the declared minimum. */
export function canRemove(elements: readonly unknown[], minimum: number): boolean {
  return elements.length > minimum;
}
