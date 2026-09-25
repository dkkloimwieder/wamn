/**
 * The app shell every web application shares.
 *
 * An application gives it a title and its screens, grouped for the
 * navigation. The shell signs in, keeps the session, lays out the page and
 * routes the address to a screen. `web/shell/vite.ts` builds the dev server
 * configuration of an application.
 */

export {
  API_BASE,
  Shell,
  type ScreenProps,
  type ShellAction,
  type ShellProps,
  type ShellRoute,
  type ShellScreen,
  type ShellSection,
} from "./shell";
export { fillPath, filledValues } from "./fill";
