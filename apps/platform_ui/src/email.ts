/** Retain native email validation when FormView coordinates submission. */
export function emailError(email: string): string {
  const input = document.createElement("input");
  input.type = "email";
  input.value = email;
  return input.validationMessage;
}
