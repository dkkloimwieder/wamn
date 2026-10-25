import { precache } from "@wfe/sync/worker";

declare const __PRECACHE_MANIFEST__: string[];

// wfe gap: precache accepts only one excluded prefix. Identity navigations
// must also bypass its offline fallback, not just its asset cache.
globalThis.addEventListener("fetch", (event: Event) => {
  const path = new URL((event as Event & { request: Request }).request.url).pathname;
  if (path === "/password" || path.startsWith("/password/")) event.stopImmediatePropagation();
});

precache(__PRECACHE_MANIFEST__, { apiPrefix: "/wamn", navigationFallback: "live.html" });
