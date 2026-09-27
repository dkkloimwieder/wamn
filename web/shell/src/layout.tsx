/**
 * The page of a signed-in session: the frame, its navigation and the toaster.
 *
 * The shell loads this module only after sign in, so the sign in page does not
 * load the frame, the sidebar or the toaster.
 */

import { A, useLocation } from "@solidjs/router";
import { type JSX } from "solid-js";

import { AppFrame, Button, Toaster, useColorMode, type FrameEntry, type FrameItem } from "@wamn/ui";

import type { ShellScreen, ShellSection } from "./shell";

export default function Layout(props: {
  readonly title: string;
  readonly sections: readonly ShellSection[];
  readonly aud: string;
  readonly signOut: () => Promise<void>;
  readonly children?: JSX.Element;
}): JSX.Element {
  const location = useLocation();
  const { colorMode, toggleColorMode } = useColorMode();
  const item = (label: string, screen: ShellScreen): FrameItem => ({
    label,
    href: screen.path,
    active: () => {
      const own = `/${props.aud}/${screen.path}`;
      return location.pathname === own || location.pathname.startsWith(`${own}/`);
    },
  });
  const navigation: FrameEntry[] = props.sections.map((section) => {
    const only = section.screens.length === 1 ? section.screens[0] : undefined;
    return only === undefined
      ? { label: section.label, items: section.screens.map((screen) => item(screen.label, screen)) }
      : item(section.label, only);
  });
  return (
    <>
      <AppFrame
        title={props.title}
        navigation={navigation}
        link={A}
        context={props.aud}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={toggleColorMode}>
              {colorMode() === "dark" ? "light mode" : "dark mode"}
            </Button>
            <Button variant="outline" size="sm" onClick={() => void props.signOut()}>
              sign out
            </Button>
          </>
        }
      >
        {props.children}
      </AppFrame>
      <Toaster />
    </>
  );
}
