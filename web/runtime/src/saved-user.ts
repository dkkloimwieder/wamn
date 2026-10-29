import type { SessionTimes } from "./session.js";

/**
 * The three offline profile settings of an application environment
 * (docs/plan/wamn-sync.md, Offline profile).
 */
export interface OfflineSettings {
  /** `sync.offline_profile`. */
  readonly profile: "session" | "local_first";
  /** `sync.pending_max_age`, an ISO 8601 duration, or undefined for none. */
  readonly pendingMaxAge: string | undefined;
  /** `sync.clear_on_sign_out`. */
  readonly clearOnSignOut: boolean;
}

/** The `session` profile defaults, used when no values are saved. */
export const SESSION_DEFAULTS: OfflineSettings = { profile: "session", pendingMaxAge: "PT1H", clearOnSignOut: true };

/** The `local_first` profile defaults. */
export const LOCAL_FIRST_DEFAULTS: OfflineSettings = {
  profile: "local_first",
  pendingMaxAge: undefined,
  clearOnSignOut: false,
};

/** An ISO 8601 duration of days, hours, minutes and seconds, in milliseconds. */
export function durationMs(duration: string): number {
  const match = /^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+(?:\.\d+)?)S)?)?$/.exec(duration);
  if (!match || duration === "P" || duration.endsWith("T")) throw new Error(`${duration} is not an ISO 8601 duration`);
  const [, days = "0", hours = "0", minutes = "0", seconds = "0"] = match;
  return ((Number(days) * 24 + Number(hours)) * 60 + Number(minutes)) * 60_000 + Number(seconds) * 1000;
}

/**
 * The last signed-in user, with the session times and the offline settings
 * read while online. It reopens local rows before renewal.
 */
export interface SavedUser extends SessionTimes {
  /** The settings read while online, or undefined when none were read. */
  readonly settings?: OfflineSettings;
}

/** The settings the Shell applies: the saved ones, or the `session` defaults. */
export function offlineSettings(user: SavedUser | undefined): OfflineSettings {
  return user?.settings ?? SESSION_DEFAULTS;
}

/** Whether an offline reopen and offline recording may run as the saved user at `now` (milliseconds). */
export function opensOffline(user: SavedUser, now: number): boolean {
  return offlineSettings(user).profile === "local_first" || user.loginExpiresAt * 1000 > now;
}

/** The last signed-in user, per application and environment. */
export function savedUser(application: string, environment: string) {
  const key = `wamn-user:${application}:${environment}`;
  return {
    load(): SavedUser | undefined {
      try {
        const text = localStorage.getItem(key);
        if (text === null) return undefined;
        const user = JSON.parse(text) as Partial<SavedUser>;
        return typeof user.userId === "string" &&
          typeof user.expiresAt === "number" &&
          typeof user.loginExpiresAt === "number"
          ? (user as SavedUser)
          : undefined;
      } catch {
        return undefined;
      }
    },
    save(user: SavedUser): void {
      try {
        localStorage.setItem(key, JSON.stringify(user));
      } catch {
        // Local rows still work for this page when storage is unavailable.
      }
    },
    clear(): void {
      try {
        localStorage.removeItem(key);
      } catch {
        // Nothing was saved when storage is unavailable.
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

/** Delete one IndexedDB database. It waits until every open connection closes. Without IndexedDB nothing was saved. */
export function deleteDatabase(name: string): Promise<void> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === "undefined") return resolve();
    const request = indexedDB.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(request.error ?? new Error(`${name} could not be deleted`));
  });
}
