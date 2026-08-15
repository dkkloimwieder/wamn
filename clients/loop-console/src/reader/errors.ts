/**
 * Typed reader errors and the result union the seam returns.
 *
 * Nothing throws across the reader seam: every read returns `ReadResult`, so
 * step 3's ErrorPanel is driven by values it can be handed directly.
 *
 * The platform draws a distinction the console must keep: a missing resource is
 * not a transport error. It comes back as a 2xx refusal carrying a literal, so
 * `not-found` below is a refusal too — it is split out only because it is the
 * one refusal with its own screen state. `refusal` covers the rest, verbatim.
 * Malformed-but-2xx bodies are step 9's typed decode and have no value here.
 */

export type ReadResource = "run" | "report" | "draft";

/** Read-side refusal literals, spelled exactly as the platform spells them. */
export type NotFoundLiteral = "run-not-found" | "report-not-found" | "draft-revision-not-found";
export type RefusalLiteral = "authorization-denied" | "unsupported-contract-version";

export type ReaderError =
  | {
      readonly kind: "not-found";
      readonly literal: NotFoundLiteral;
      readonly resource: ReadResource;
      readonly id: string;
      /** Only a draft not-found carries one. */
      readonly revision: number | null;
    }
  | {
      readonly kind: "refusal";
      readonly literal: RefusalLiteral;
      /** Whatever the refusal carried alongside its literal, already flattened for display. */
      readonly detail: string | null;
    }
  | {
      readonly kind: "network";
      /** The transport never got an HTTP response back. */
      readonly message: string;
    };

export type ReadResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly error: ReaderError };

export function ok<T>(value: T): ReadResult<T> {
  return { ok: true, value };
}

export function err<T>(error: ReaderError): ReadResult<T> {
  return { ok: false, error };
}
