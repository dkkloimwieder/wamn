/**
 * The read store of one transport (`docs/architecture/execution.md`).
 *
 * It holds the last reply of each read, keyed by the operation and the
 * canonical target, so equal reads on one page send one request. The response
 * headers decide what it keeps: `Cache-Control: max-age` keeps a reply fresh,
 * `no-cache` keeps it only to revalidate, and `no-store` keeps nothing. A stale
 * reply with an ETag revalidates with If-None-Match, and a 304 reuses it.
 *
 * A write marks every stored reply stale, because the browser cannot tell
 * which models a write changed. An unchanged read then costs one 304.
 */

import type { HttpReply } from "./transport.js";

/** One read reply with the two response headers that the store reads. */
export interface ReadReply extends HttpReply {
  readonly cacheControl: string | null;
  readonly etag: string | null;
}

/** The last reply of one read, and the request that is in flight for it. */
interface Entry {
  reply: HttpReply | null;
  etag: string | null;
  freshUntil: number;
  pending: { readonly generation: number; readonly reply: Promise<HttpReply> } | null;
}

/** The store that one transport owns. */
export interface ReadStore {
  /**
   * The reply of the read at `key`. `send` makes the request, with the stored
   * tag or null. `keep` tells whether a 200 reply is an outcome to store.
   */
  read(
    key: string,
    send: (ifNoneMatch: string | null) => Promise<ReadReply>,
    keep: (reply: HttpReply) => boolean,
  ): Promise<HttpReply>;
  /** Marks every stored reply stale, after a write. */
  invalidate(): void;
  /** Empties the store when the session differs from the one it holds. */
  belongTo(session: string | null): void;
}

export function createReadStore(): ReadStore {
  let entries = new Map<string, Entry>();
  let session: string | null = null;
  // A write adds one. A read that started before a write shares no request
  // with a later read, and the reply it stores is stale.
  let generation = 0;

  /** Sends the request of `entry`, and stores its reply when the headers allow. */
  async function settle(
    key: string,
    entry: Entry,
    send: (ifNoneMatch: string | null) => Promise<ReadReply>,
    keep: (reply: HttpReply) => boolean,
  ): Promise<HttpReply> {
    const started = generation;
    const previous = entry.reply;
    let response: ReadReply;
    try {
      response = await send(previous === null ? null : entry.etag);
    } catch (error) {
      if (entries.get(key) === entry) {
        entries.delete(key);
      }
      throw error;
    }
    const reply = response.status === 304 && previous !== null ? previous : response;
    // A later read of the same key owns the key now, and stores its own reply.
    if (entries.get(key) !== entry) {
      return reply;
    }
    const directives = (response.cacheControl ?? "")
      .split(",")
      .map((directive) => directive.trim().toLowerCase());
    let maxAge = 0;
    for (const directive of directives) {
      const age = /^max-age=(\d+)$/.exec(directive);
      if (age !== null) {
        maxAge = Number(age[1]);
      }
    }
    if (directives.includes("no-cache")) {
      maxAge = 0;
    }
    const etag = response.etag ?? (reply === previous ? entry.etag : null);
    // A reply that is never fresh and has no tag saves no request, so it is not kept.
    if (
      reply.status !== 200 ||
      directives.includes("no-store") ||
      (etag === null && maxAge === 0) ||
      !keep(reply)
    ) {
      entries.delete(key);
      return reply;
    }
    entry.reply = reply;
    entry.etag = etag;
    entry.freshUntil = started === generation ? Date.now() + maxAge * 1000 : 0;
    entry.pending = null;
    return reply;
  }

  return {
    read(key, send, keep) {
      const stored = entries.get(key);
      if (stored?.pending != null && stored.pending.generation === generation) {
        return stored.pending.reply;
      }
      if (stored?.reply != null && Date.now() < stored.freshUntil) {
        return Promise.resolve(stored.reply);
      }
      const entry: Entry = {
        reply: stored?.reply ?? null,
        etag: stored?.etag ?? null,
        freshUntil: 0,
        pending: null,
      };
      entries.set(key, entry);
      const reply = settle(key, entry, send, keep);
      entry.pending = { generation, reply };
      return reply;
    },
    invalidate() {
      generation += 1;
      for (const entry of entries.values()) {
        entry.freshUntil = 0;
      }
    },
    belongTo(owner) {
      if (owner !== session) {
        session = owner;
        generation += 1;
        entries = new Map();
      }
    },
  };
}
