/**
 * The frame of one gallery section, and of one state inside it.
 */

import type { JSX, ParentProps } from "solid-js";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@wamn/ui";

/** One gallery section: its title, the name of the export it shows, and its states. */
export function Section(props: ParentProps<{ title: string; name: string }>): JSX.Element {
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>{props.title}</h2>
        </CardTitle>
        <CardDescription class="font-mono">{props.name}</CardDescription>
      </CardHeader>
      <CardContent>{props.children}</CardContent>
    </Card>
  );
}

/** One state of a component, under the name of that state. */
export function State(props: ParentProps<{ name: string }>): JSX.Element {
  return (
    <div class="flex min-w-0 flex-col gap-2">
      <p class="text-xs text-muted-foreground">{props.name}</p>
      {props.children}
    </div>
  );
}
