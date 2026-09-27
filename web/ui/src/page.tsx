/**
 * The light page parts, apart from the frame so that the sign in page loads
 * without the sidebar.
 *
 * CardPage is one card in the middle of an empty page, for signing in and for
 * an address with no page. ScreenActions is the row of buttons above a screen.
 */

import { type JSX } from "solid-js";

import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";

export interface ScreenActionsProps {
  readonly children: JSX.Element;
}

export function ScreenActions(props: ScreenActionsProps): JSX.Element {
  return <div class="mb-4 flex flex-wrap gap-2">{props.children}</div>;
}

export interface CardPageProps {
  readonly title: string;
  readonly children: JSX.Element;
}

export function CardPage(props: CardPageProps): JSX.Element {
  return (
    <div class="flex min-h-screen items-center justify-center p-4">
      <Card class="w-full max-w-sm">
        <CardHeader>
          <CardTitle>
            <h1>{props.title}</h1>
          </CardTitle>
        </CardHeader>
        <CardContent class="flex flex-col gap-6">{props.children}</CardContent>
      </Card>
    </div>
  );
}
