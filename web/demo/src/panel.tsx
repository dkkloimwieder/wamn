/**
 * The two frames every page of the demo draws around a screen.
 */

import type { JSX } from "solid-js";

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@wamn/ui";

/** One screen on the page: a card with its title and the operation it calls. */
export function Panel(props: { title: string; operation: string; children: JSX.Element }) {
  return (
    <Card class="min-w-0">
      <CardHeader>
        <CardTitle>
          <h2>{props.title}</h2>
        </CardTitle>
        <CardDescription class="font-mono">{props.operation}</CardDescription>
      </CardHeader>
      <CardContent class="min-w-0">{props.children}</CardContent>
    </Card>
  );
}

/** What a screen shows until the record it needs is chosen. */
export function Waiting(props: { children: JSX.Element }) {
  return (
    <p class="border border-dashed p-8 text-center text-sm text-muted-foreground">
      {props.children}
    </p>
  );
}
