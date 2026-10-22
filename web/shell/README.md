# Shared web tooling

This directory keeps the Vite application configuration and the Playwright dependency for browser tests.
The package is named `@wamn/vite` and contains no UI components or application shell.

[`vite.ts`](vite.ts) supplies development proxies, `/config.json`, generated client aliases, and the build output directory.
The [Receiving Vite configuration](../../apps/wamn_receiving/web/vite.config.ts) uses it.
The [platform browser test](../../apps/platform_ui/test/browser.mjs) and [Receiving browser test](../../apps/wamn_receiving/web/test/browser.mjs) use Playwright from this package.
The [platform UI](../../apps/platform_ui/README.md) owns the shared application shell.

Install dependencies with `pnpm install` at the repository root.
To type-check the Vite helper, run:

```bash
pnpm --dir web/shell check
```
