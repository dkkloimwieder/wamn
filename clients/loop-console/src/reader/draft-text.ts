import type { DraftParse } from "./types";

/**
 * Facts the console derives from a draft's exact bytes, because the platform
 * stores neither: drafts have no `byte_length` column and `read-draft` returns
 * no parse verdict. Shared by the fixture reader and step 9's HTTP reader, both
 * of which get nothing but `definition` from the contract.
 */

/** UTF-8 bytes, not JavaScript code units — §2.4's `4,812 bytes` is a byte count. */
export function byteLength(definition: string): number {
  return new TextEncoder().encode(definition).length;
}

/** Strips the machine tail so the message reads as a sentence beside its line number. */
const positionTail = / in JSON at position \d+(?: \(line \d+ column \d+\))?$/;
/**
 * V8's other shape quotes the source back — `Unexpected token 'o', "{…}" is not
 * valid JSON`, with `...` where it elided. Stripped so §2.4's one-line
 * `does not parse — …` label can never print the draft it is describing.
 */
const quotedSource = /, (?:\.\.\.)?"[\s\S]*"(?:\.\.\.)? is not valid JSON$/;
/** `(line 41 column 20)` on V8, `at line 41 column 20` on Firefox. */
const located = /(?:\(|at )line (\d+) column \d+/;

/**
 * A half-finished draft is a normal object, not an error condition — the
 * platform stores it unparsed on purpose, so the console reports on it rather
 * than refusing it.
 */
export function parseDraft(definition: string): DraftParse {
  try {
    JSON.parse(definition);
    return { ok: true };
  } catch (error) {
    if (!(error instanceof SyntaxError)) {
      throw error;
    }
    const match = located.exec(error.message);
    return {
      ok: false,
      // Only some shapes carry a position — the quoted-source shape and
      // `Unexpected end of JSON input` carry none. Null rather than a guess: a
      // guessed line tints the wrong line, or a line that does not exist.
      line: match === null ? null : Number(match[1]),
      message: error.message.replace(positionTail, "").replace(quotedSource, ""),
    };
  }
}
