import { precache } from "@wfe/sync/worker";

declare const __PRECACHE_MANIFEST__: string[];

precache(__PRECACHE_MANIFEST__, { bypassPrefixes: ["/wamn", "/password"], navigationFallback: "live.html" });
