import { collection, type Feed, type Persistence, type Row } from "@wfe/sync";
import type { PackagingRow, PackagingScope } from "./generated/wamn_wms/packaging.js";
import { packagingKey } from "./generated/wamn_wms/packaging.js";
import { locationKey, type LocationRow } from "./generated/wamn_wms/location.js";
import type { StatusFilter } from "./order.js";

export function packagingScope(filter: StatusFilter): PackagingScope {
  return filter === "all" ? {} : { filter: { status: [filter] } };
}

/** UTF-8 JSON bytes for the saved entry and its key, before IndexedDB overhead. */
export function collectionBytes(key: string, rows: unknown[], cursor: string, savedAt: string): number {
  return new TextEncoder().encode(JSON.stringify({ key, rows, cursor, saved_at: savedAt })).byteLength;
}

export function requireStorage(bytes: number, estimate: StorageEstimate): number {
  const needed = Math.ceil(bytes * 1.2);
  if (estimate.quota === undefined || estimate.usage === undefined) {
    throw new Error(`Storage estimate unavailable. Need ${needed} bytes including 20% headroom.`);
  }
  const available = Math.max(0, estimate.quota - estimate.usage);
  if (available < needed) {
    throw new Error(`Need ${needed} bytes including 20% headroom; ${available} bytes available.`);
  }
  return needed;
}

export function needsConnection(online: boolean, local: boolean | undefined): boolean {
  return !online && local === false;
}

type Page<T> = { rows: T[]; cursor: string; complete?: boolean };

/** TanStack adds $ metadata to collection rows. Save only model fields. */
function modelRows<T extends Row>(rows: T[]): T[] {
  return rows.map((row) => Object.fromEntries(Object.entries(row).filter(([name]) => !name.startsWith("$")))) as T[];
}

/** Keep offline reads local, including a saved empty collection.
 * wfe gaps wamn-qio9a (network list after restore when offline) and
 * wamn-4qimg (empty saved collection and saved_at not restored). */
function storedCollection<T extends Row & { rowVersion: number }, S>(
  feed: Feed<T, S>, scope: S, key: string, persistence: Persistence, pollMs: number,
  online: () => boolean, initial?: Page<T>,
) {
  let staged = initial;
  let cursor = "";
  let asOf: string | undefined;
  let local: boolean | undefined;
  const saved = {
    ...persistence,
    async load(key: string) {
      const entry = await persistence.load(key);
      local = entry !== undefined;
      asOf = entry?.saved_at;
      return entry;
    },
    // An offline read must not move the saved "as of" time forward.
    async save(key: string, rows: unknown[], cursor: string) {
      if (online()) {
        await persistence.save(key, modelRows(rows as T[]), cursor);
        local = true;
      }
    },
  };
  const c = collection<T, S>({
    async list(scope, signal) {
      let page = staged;
      if (page === undefined && !online()) {
        const entry = await persistence.load(key);
        local = entry !== undefined;
        if (!entry) throw new Error("needs a connection");
        asOf = entry.saved_at;
        page = { rows: entry.rows as T[], cursor: entry.cursor, complete: true };
      } else {
        page ??= await feed.list(scope, signal);
        asOf = new Date().toISOString();
      }
      cursor = page.cursor;
      return page;
    },
    moved: async () => online(),
    async changes(scope, since, limit, signal) {
      if (!online()) return { rows: [], next_cursor: since, complete: true };
      const page = await feed.changes(scope, since, limit, signal);
      cursor = page.next_cursor;
      asOf = new Date().toISOString();
      return page;
    },
  }, scope, { key, persistence: saved, pollMs, version: (row) => row.rowVersion });
  return {
    c,
    currentAt: () => asOf,
    local: () => local,
    async fill(page: Page<T>) {
      staged = page;
      try {
        await c.utils.refill();
        if (c.utils.error()) throw c.utils.error();
        // wfe gap wamn-k1jb3: saves run later and hide failures. Await durability before completion.
        await persistence.save(key, modelRows(c.toArray), cursor);
        local = true;
      } finally {
        staged = undefined;
      }
    },
  };
}

export interface OfflineResult {
  packaging: { count: number; bytes: number; asOf: string };
  location: { count: number; bytes: number; asOf: string };
  needed: number;
}

/** The packaging scope and the whole location model share one user-owned store. */
export function packagingOffline(options: {
  packaging: Feed<PackagingRow, PackagingScope>;
  location: Feed<LocationRow, Record<string, never>>;
  scope: PackagingScope;
  persistence: Persistence;
  pollMs?: number;
  online?: () => boolean;
  estimate?: () => Promise<StorageEstimate>;
}) {
  const { persistence, scope } = options;
  const online = options.online ?? (() => navigator.onLine);
  const pollMs = options.pollMs ?? 10_000;
  const key = packagingKey(scope);
  const packaging = storedCollection(options.packaging, scope, key, persistence, pollMs, online);
  type Locations = ReturnType<typeof storedCollection<LocationRow, Record<string, never>>>;
  let location: Locations | undefined;
  let closed = false;
  const restored = persistence.load(locationKey({})).then((entry) => {
    if (entry && !closed) location = storedCollection(options.location, {}, locationKey({}), persistence, pollMs, online);
  }).catch(() => undefined);
  return {
    packaging,
    get location() { return location; },
    restored,
    async takeOffline(): Promise<OfflineResult> {
      if (!online()) throw new Error("needs a connection");
      await restored;
      // Read the full chosen set into memory to measure its actual serialized size.
      // Reuse these pages for fill, so the estimate needs no second list read.
      const p = await options.packaging.list(scope);
      const l = await options.location.list({});
      if (p.complete === false || l.complete === false) throw new Error("The chosen set is incomplete.");
      const asOf = new Date().toISOString();
      const packagingBytes = collectionBytes(key, p.rows, p.cursor, asOf);
      const locationBytes = collectionBytes(locationKey({}), l.rows, l.cursor, asOf);
      const estimate = await (options.estimate ?? (() => navigator.storage.estimate()))();
      const needed = requireStorage(packagingBytes + locationBytes, estimate);
      if (closed) throw new Error("The screen closed before fill.");
      await packaging.fill(p);
      if (closed) throw new Error("The screen closed before fill.");
      location ??= storedCollection(options.location, {}, locationKey({}), persistence, pollMs, online, l);
      await location.fill(l);
      return {
        packaging: { count: packaging.c.size, bytes: packagingBytes, asOf: packaging.currentAt()! },
        location: { count: location.c.size, bytes: locationBytes, asOf: location.currentAt()! },
        needed,
      };
    },
    async close() {
      closed = true;
      await packaging.c.cleanup();
      await location?.c.cleanup();
    },
  };
}
