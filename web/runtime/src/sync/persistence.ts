/** The saved rows and feed cursor of one collection. */
interface SavedRows {
  rows: unknown[];
  cursor: string;
  saved_at: string;
}

/** IndexedDB storage for the @wfe/sync Persistence interface. */
export function rowsPersistence(database: string) {
  async function transact<T>(
    mode: IDBTransactionMode,
    run: (store: IDBObjectStore) => IDBRequest<T>,
  ): Promise<T> {
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
    load(key: string): Promise<SavedRows | undefined> {
      return transact("readonly", (store) => store.get(key) as IDBRequest<SavedRows | undefined>);
    },
    async save(key: string, rows: unknown[], cursor: string): Promise<void> {
      await transact("readwrite", (store) => store.put({ rows, cursor, saved_at: new Date().toISOString() }, key));
    },
    async remove(key: string): Promise<void> {
      await transact("readwrite", (store) => store.delete(key));
    },
  };
}
