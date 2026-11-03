# Shared web tooling

This directory keeps the Vite application configuration and the Playwright dependency for browser tests.
The package is named `@wamn/vite` and contains no UI components or application shell.

[`vite.ts`](vite.ts) is the package entry. It supplies development proxies, `/config.json`, and the build output directory.
The [Receiving Vite configuration](../../apps/wamn_receiving/web/vite.config.ts) and the sync-bench Vite configuration import it as `@wamn/vite`.
Each application declares the workspace packages that it imports, and imports each one through its package entry.
The [platform browser test](../../apps/platform_ui/test/browser.mjs) and [Receiving browser test](../../apps/wamn_receiving/web/test/browser.mjs) use Playwright from this package.
The [platform UI](../../apps/platform_ui/README.md) owns the shared application shell.

Install dependencies with `pnpm install` at the repository root.
To type-check the Vite helper, run:

```bash
pnpm --dir web/shell check
```
