/**
 * The web runtime that generated browser clients call.
 *
 * It owns the wire contract, the transport that turns one reply into one
 * outcome, the three values a caller supplies without asking the operator, and
 * the helpers that a generated component calls: the page state, the draft
 * members, and the cell text.
 */

export * from "./wire.js";
export * from "./transport.js";
export * from "./supplied.js";
export * from "./page.js";
export * from "./draft.js";
export * from "./cell.js";
