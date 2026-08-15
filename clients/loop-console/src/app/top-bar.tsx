import type { JSX } from "@solidjs/web";

import { proxyTarget } from "../config";
import { readStatus, readStatusWord } from "./read-status";

export function TopBar(): JSX.Element {
  const word = () => readStatusWord(readStatus());
  return (
    <header class="top-bar">
      <span class="wordmark frame">wamn loop</span>
      <span class="proxy-target">{proxyTarget}</span>
      <span class="status-dot" data-status={readStatus()} title={word()} aria-hidden="true" />
      <span class="visually-hidden">read status: {word()}</span>
      <span class="jump-hint label">⌘K</span>
    </header>
  );
}
