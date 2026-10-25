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
