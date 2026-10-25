/** The outbox database for one application, environment and user. */
export function outboxDatabase(application: string, environment: string, userId: string): string {
  return `wamn-outbox:${application}:${environment}:${userId}`;
}
