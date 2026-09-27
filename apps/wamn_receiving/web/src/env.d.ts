/** The values `wamn web upload` gives the build, through `web/shell/vite.ts`. */
interface ImportMeta {
  readonly env: { readonly WAMN_ORG: string; readonly WAMN_PROJECT: string };
}
