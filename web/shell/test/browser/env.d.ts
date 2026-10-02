/** The org and the project the browser test page signs in to, from `vite.config.ts`. */
interface ImportMeta {
  readonly env: { readonly WAMN_ORG: string; readonly WAMN_PROJECT: string };
}
