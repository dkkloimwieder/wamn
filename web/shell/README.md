# Shared web tooling

This directory keeps the Vite application configuration and the Playwright dependency for browser tests.
The package is named `@wamn/vite` and contains no UI components or application shell.

[`vite.ts`](vite.ts) is the package entry `@wamn/vite`, and it holds the Vite policy of every Solid application.
`solidApplication` supplies the Solid compiler, the browser resolution with one copy of Solid, the Solid version test and the test environment.
`applicationConfig` supplies the development proxies and the build output directory.
Set `WAMN_ROUTE_URL` to the installed URL with its app mount.
Open the local dev server at the same mount.
The proxy sends mounted API and configuration requests to web. Org control and identity requests use host-root paths.
Set `mounted: true` for a published app build.
Put `<!--wamn-app-base-->` in the HTML head before relative asset references.
Web replaces the marker with the mount so relative assets work on deep links.
`workerBuild` writes the @wfe/sync worker as `sw.js` beside the pages, and `precacheManifest` gives the worker its file list.
Each application keeps its entries, port, output directory, public assets and cache exclusions.

[`bundle.ts`](bundle.ts), the entry `@wamn/vite/bundle`, holds the bundle make sure that the platform and Receiving bundle tests run.
It builds an application and makes sure that Node parses every emitted script, the worker included.

Each application declares the workspace packages that it imports, and imports each one through its package entry.
The [platform browser test](../../apps/platform_ui/test/browser.mjs) and [Receiving browser test](../../apps/wamn_receiving/web/test/browser.mjs) use Playwright from this package.
The [platform UI](../../apps/platform_ui/README.md) owns the shared application shell.

Install dependencies with `pnpm install` at the repository root.
To compile the Vite helper, run:

```bash
pnpm --dir web/shell lint
```
