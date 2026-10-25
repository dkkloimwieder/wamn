/** The outbox database for one application, environment and user. */
export function outboxDatabase(application: string, environment: string, userId: string): string {
  return `wamn-outbox:${application}:${environment}:${userId}`;
}

/** The rows database for one application, environment and user. */
export function rowsDatabase(application: string, environment: string, userId: string): string {
  return `wamn-rows:${application}:${environment}:${userId}`;
}
