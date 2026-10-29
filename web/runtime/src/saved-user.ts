/** The last signed-in user, used only to reopen local rows before renewal. */
export function savedUser(application: string, environment: string) {
  const key = `wamn-user:${application}:${environment}`;
  return {
    load(): string | undefined {
      try {
        return localStorage.getItem(key) ?? undefined;
      } catch {
        return undefined;
      }
    },
    save(id: string): void {
      try {
        localStorage.setItem(key, id);
      } catch {
        // Local rows still work for this page when storage is unavailable.
      }
    },
  };
}

/**
 * What the last signed-in user held: the operation references, and whether the
 * user was an admin. It decides only which screens show before renewal. The
 * host still authorizes every call.
 */
export interface SavedView {
  readonly userId: string;
  readonly held: readonly string[];
  readonly admin: boolean;
}

/** The view of the last signed-in user, per application and environment. */
export function savedView(application: string, environment: string) {
  const key = `wamn-view:${application}:${environment}`;
  return {
    /** The saved view of this user, or undefined when another user saved the last one. */
    load(userId: string): SavedView | undefined {
      try {
        const text = localStorage.getItem(key);
        const view = text === null ? undefined : (JSON.parse(text) as SavedView);
        return view?.userId === userId ? view : undefined;
      } catch {
        return undefined;
      }
    },
    save(view: SavedView): void {
      try {
        localStorage.setItem(key, JSON.stringify(view));
      } catch {
        // The page signs in again when storage is unavailable.
      }
    },
  };
}
