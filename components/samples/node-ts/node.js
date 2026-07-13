// S4 JS/JCO bench node — the "interpreted" arm. Implements the same minimal
// wamn:node handler as components/node-rs, driven by the JSON config on the
// run-context, so the interpreted-vs-composed gap (docs/p0-exit-criteria.md S4)
// compares like with like. Componentized with `jco componentize` (StarlingMonkey).
//
// Modes (config {"mode":..,"wait_ns":N,"iters":N}):
//   noop    — echo the input.
//   io      — wait via the host `wait-ns` import (identical floor to the Rust
//             node; the async host sleep suspends the whole guest at the wasm
//             fiber boundary, so JS "blocks" here without JSPI).
//   compute — a 32-bit FNV-1a hashing loop (Math.imul), the CPU-bound workload
//             where the JS-vs-native gap is expected to be large.

import { waitNs } from 'wamn:nodebench/host@0.1.0';

const enc = new TextEncoder();

function compute(bytes, iters) {
  let acc = 0x811c9dc5 >>> 0;
  for (let i = 0; i < iters; i++) {
    acc = (acc ^ i) >>> 0;
    for (let j = 0; j < bytes.length; j++) {
      acc = Math.imul(acc ^ bytes[j], 0x01000193) >>> 0;
    }
    acc = ((acc << 5) | (acc >>> 27)) >>> 0;
  }
  return acc >>> 0;
}

export const handler = {
  run(ctx, input) {
    const inline = input.tag === 'inline' ? input.val : '';
    // design-note 9b: the dynamic custom node parses its JSON config.
    const cfg = JSON.parse(ctx.config);

    let acc = 0;
    switch (cfg.mode) {
      case 'noop':
        break;
      case 'io':
        // wait_ns is well under 2^53 (µs-to-ms range); BigInt() is exact.
        waitNs(BigInt(cfg.wait_ns));
        break;
      case 'compute':
        acc = compute(enc.encode(inline), cfg.iters);
        break;
      default:
        // Bench inputs are always valid; a bad mode is a hard error.
        throw new Error('unknown mode ' + cfg.mode);
    }

    const out = JSON.stringify({ parse_ns: -1, acc, n: inline.length, mode: cfg.mode });
    // Frozen 0.1 (5.4): run returns an emission record; an omitted `port`
    // lowers to the absent option = the "main" port.
    return { payload: { tag: 'inline', val: out } };
  },
};
