import { precache } from "@wfe/sync/worker";

declare const __PRECACHE_MANIFEST__: string[];

// The release answers at /api and the identity service at /password. Neither is cached.
precache(__PRECACHE_MANIFEST__, { bypassPrefixes: ["/api", "/password"] });
