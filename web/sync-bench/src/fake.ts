import type { Change, Feed } from "@wfe/sync";
import type { PackagingRow, PackagingScope } from "./generated/wamn_wms/packaging.js";

const statuses = ["available", "consumed", "held"] as const;
const types = ["bin", "case", "loose", "pallet", "tote"] as const;
const locations = Array.from({ length: 50 }, (_, index) => uuid(index + 1, 0x4c4f43));
const actor = uuid(1, 0x555352);

/**
 * A seeded generator, so every repeat and both shapes see the same rows and
 * the same change batches.
 */
export function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let next = Math.imul(state ^ (state >>> 15), 1 | state);
    next = (next + Math.imul(next ^ (next >>> 7), 61 | next)) ^ next;
    return ((next ^ (next >>> 14)) >>> 0) / 4294967296;
  };
}

/** A UUIDv7-shaped id whose leading bits order by `index`. */
function uuid(index: number, tag: number): string {
  const hex = index.toString(16).padStart(12, "0");
  const tail = tag.toString(16).padStart(12, "0");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-7000-8000-${tail}`;
}

function at(index: number): string {
  return new Date(Date.UTC(2026, 0, 1) + index * 60_000).toISOString();
}

function row(index: number, random: () => number): PackagingRow {
  const time = at(index);
  return {
    id: uuid(index + 1, 0x504b47),
    rowVersion: 1,
    createdAt: time,
    createdBy: actor,
    locatedAt: time,
    locationId: locations[Math.floor(random() * locations.length)]!,
    packagingCode: `PK-${String(index + 1).padStart(7, "0")}`,
    status: statuses[Math.floor(random() * statuses.length)]!,
    type: types[Math.floor(random() * types.length)]!,
    updatedAt: time,
    updatedBy: actor,
  } as PackagingRow;
}

/** `count` synthetic rows in the generated row type. */
export function makeRows(count: number, seed = 1): PackagingRow[] {
  const random = seeded(seed);
  return Array.from({ length: count }, (_, index) => row(index, random));
}

/** A feed that answers `list` from memory. `changes` has nothing yet. */
export function fakeFeed(rows: () => PackagingRow[]): Feed<PackagingRow, PackagingScope> {
  return {
    async list() {
      return { rows: rows(), cursor: "none" };
    },
    async changes(_scope, cursor) {
      return { rows: [], next_cursor: cursor, complete: true };
    },
  };
}

/**
 * One batch of 100 changes against `current`: 70 updates, 20 inserts and 10
 * deletes, on distinct rows. `next` numbers the inserted rows.
 */
export function randomChanges(
  current: readonly PackagingRow[],
  random: () => number,
  next: number,
): Change<PackagingRow>[] {
  const picked = new Set<number>();
  const pick = () => {
    let index = Math.floor(random() * current.length);
    while (picked.has(index)) index = (index + 1) % current.length;
    picked.add(index);
    return current[index]!;
  };
  const changes: Change<PackagingRow>[] = [];
  for (let step = 0; step < 70; step++) {
    const old = pick();
    const version = old.rowVersion + 1;
    const data = {
      ...old,
      rowVersion: version,
      status: statuses[Math.floor(random() * statuses.length)]!,
      updatedAt: at(next + step),
    } as PackagingRow;
    changes.push({ id: old.id, row_version: version, deleted: false, data });
  }
  for (let step = 0; step < 10; step++) {
    const old = pick();
    changes.push({ id: old.id, row_version: old.rowVersion + 1, deleted: true });
  }
  for (let step = 0; step < 20; step++) {
    const data = row(next + step, random);
    changes.push({ id: data.id, row_version: 1, deleted: false, data });
  }
  return changes;
}
