/** The saved rows and feed cursor of one collection. */
interface SavedRows {
  rows: unknown[];
  cursor: string;
  saved_at: string;
}

/** IndexedDB storage for the @wfe/sync Persistence interface. */
export function rowsPersistence(database: string, shapeDigests: readonly string[]) {
  const known = new Set(shapeDigests);
  let prepared: Promise<unknown> | undefined;

  function current(key: IDBValidKey): boolean {
    if (typeof key !== "string") return false;
    try {
      const parts: unknown = JSON.parse(key);
      return Array.isArray(parts) && parts.length === 3 && known.has(parts[1]);
    } catch {
      // Saved keys from before shape digests need a fresh fill too.
      return false;
    }
  }

  function prepare(): Promise<unknown> {
    return (prepared ??= transact("readwrite", (store) => {
      const request = store.openKeyCursor();
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        if (!current(cursor.key)) store.delete(cursor.primaryKey);
        cursor.continue();
      };
      return request;
    }));
  }

  async function transact<T>(mode: IDBTransactionMode, run: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(database, 1);
      request.onupgradeneeded = () => request.result.createObjectStore("rows");
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      return await new Promise<T>((resolve, reject) => {
        const transaction = db.transaction("rows", mode);
        const request = run(transaction.objectStore("rows"));
        transaction.oncomplete = () => resolve(request.result);
        transaction.onabort = () => reject(transaction.error ?? new Error("Row storage transaction aborted"));
        transaction.onerror = () => reject(transaction.error);
      });
    } finally {
      db.close();
    }
  }

  return {
    async load(key: string): Promise<SavedRows | undefined> {
      await prepare();
      return transact("readonly", (store) => store.get(key) as IDBRequest<SavedRows | undefined>);
    },
    async save(key: string, rows: unknown[], cursor: string): Promise<void> {
      await prepare();
      await transact("readwrite", (store) => store.put({ rows, cursor, saved_at: new Date().toISOString() }, key));
    },
    async remove(key: string): Promise<void> {
      await prepare();
      await transact("readwrite", (store) => store.delete(key));
    },
  };
}
