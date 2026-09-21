/**
 * The web runtime that generated browser clients call.
 *
 * It owns the wire contract, the transport that turns one reply into one
 * outcome, and the three values a caller supplies without asking the operator.
 */

export * from "./wire.js";
export * from "./transport.js";
export * from "./supplied.js";
