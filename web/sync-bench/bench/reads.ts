/** Classify the packaging reads that the live driver observes. */
export function packagingRead(url: URL): "list" | "changes" | undefined {
  if (!url.pathname.endsWith("/packaging/query")) return undefined;
  return url.searchParams.has("feed") ? "changes" : "list";
}

declare global {
  interface Window {
    listReads: { started: number; resolved: number };
    releaseList: () => void;
  }
}

/** Installed in the page before its scripts, with the same classifier as the proxy. */
export function trackListReads(read: typeof packagingRead): void {
  const fetch = window.fetch.bind(window);
  window.listReads = { started: 0, resolved: 0 };
  window.fetch = async (...args) => {
    const input = args[0];
    const url = new URL(typeof input === "string" || input instanceof URL ? input : input.url, location.href);
    if (read(url) !== "list") return fetch(...args);
    window.listReads.started += 1;
    // Hold only the list, so saved rows must paint without its response.
    if (sessionStorage.getItem("hold-list") === "yes") {
      await new Promise<void>((resolve) => {
        window.releaseList = resolve;
      });
    }
    try {
      return await fetch(...args);
    } finally {
      window.listReads.resolved += 1;
    }
  };
}
