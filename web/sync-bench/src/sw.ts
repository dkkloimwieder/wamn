import { precache } from "@wfe/sync/worker";

declare const __PRECACHE_MANIFEST__: string[];

// wfe 5a gap wamn-p3gl3: remove this local filter and listener when precache supports
// bypassPrefixes. Both paths must bypass installation and the fetch handler.
const bypass = (url: string) => /^\/(wamn|password)(\/|$)/.test(new URL(url, location.href).pathname);
globalThis.addEventListener("fetch", (event: Event) => {
  if (bypass((event as Event & { request: Request }).request.url)) event.stopImmediatePropagation();
});

precache(__PRECACHE_MANIFEST__.filter((url) => !bypass(url)), { apiPrefix: "/wamn", navigationFallback: "live.html" });
