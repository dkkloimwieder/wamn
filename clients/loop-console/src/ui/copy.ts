/**
 * Copy exact text, reporting whether it landed so the affordance can say so.
 *
 * Never throws. The clipboard API is permission-gated and absent outside secure
 * contexts (and in jsdom), and a copy that did not happen is a state the
 * affordance renders, not an error its caller handles.
 */
export async function copyText(text: string): Promise<boolean> {
  const clipboard: Clipboard | undefined = navigator.clipboard;
  if (clipboard === undefined) {
    return false;
  }
  try {
    await clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
