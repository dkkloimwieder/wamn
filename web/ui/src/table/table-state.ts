/**
 * The state of one QueryTable: its three parts, the views saved from it, and
 * its place in the URL (wamn-9v2r.2, wamn-5pzt).
 *
 * The state starts from the table definition, or from the URL when the table
 * has a URL key, before the first load. A table with a URL key keeps the URL
 * in step with its state, and leaves the other keys as they were written. A
 * view is a named copy of the state, kept in memory for the session. Applying
 * a view or the definition's state drops the fields the table does not
 * declare, and tells the caller what the query was before.
 */

import { createEffect, createSignal } from "solid-js";

import type { GridViewState } from "./grid-view";
import type { QueryViewState } from "./query-view";
import type { SetViewState } from "./set-view";
import { declaredView, decodeView, encodeView, type NamedView, type TableView, type ViewDeclaration } from "./view";

export function createTableState(options: {
  readonly declaration: ViewDeclaration;
  readonly defaults: TableView;
  readonly urlKey?: string | undefined;
  /** Called after a view applies, with the query before it. */
  readonly onApply: (before: QueryViewState, after: TableView) => void;
}) {
  const { declaration, defaults, urlKey } = options;
  const read =
    urlKey === undefined
      ? { view: defaults, ignored: [] }
      : decodeView(urlKey, new URLSearchParams(window.location.search), defaults, declaration);
  const [view, setView] = createSignal<TableView>(read.view);
  const [views, setViews] = createSignal<readonly NamedView[]>([]);
  const [chosen, setChosen] = createSignal<string | null>(null);

  const apply = (next: TableView) => {
    const before = view().query;
    const applied = declaredView(next, declaration);
    setView(applied);
    options.onApply(before, applied);
  };

  if (urlKey !== undefined) {
    createEffect(() => {
      const others = window.location.search
        .slice(1)
        .split("&")
        .filter((part) => part !== "" && !part.startsWith(`${encodeURIComponent(urlKey)}.`));
      const own = new URLSearchParams(encodeView(urlKey, view(), defaults, declaration)).toString();
      const query = [...others, ...(own === "" ? [] : [own])].join("&");
      window.history.replaceState(
        window.history.state,
        "",
        `${window.location.pathname}${query === "" ? "" : `?${query}`}${window.location.hash}`,
      );
    });
  }

  return {
    view,
    /** What the URL named that the table does not have. */
    ignored: read.ignored,
    setGrid: (grid: GridViewState) => setView((current) => ({ ...current, grid })),
    setSet: (set: SetViewState) => setView((current) => ({ ...current, set })),
    setQuery: (change: Partial<QueryViewState>) =>
      setView((current) => ({ ...current, query: { ...current.query, ...change } })),
    /** The saved views, as the view bar offers them. */
    views: {
      names: () => views().map((named) => named.name),
      chosen,
      pick: (name: string) => {
        const named = views().find((candidate) => candidate.name === name);
        if (named !== undefined) {
          apply(named.view);
          setChosen(name);
        }
      },
      save: (name: string) => {
        const saved = view();
        setViews((current) =>
          current.some((named) => named.name === name)
            ? current.map((named) => (named.name === name ? { name, view: saved } : named))
            : [...current, { name, view: saved }],
        );
        setChosen(name);
      },
      rename: (from: string, to: string) => {
        setViews((current) => current.map((named) => (named.name === from ? { ...named, name: to } : named)));
        setChosen(to);
      },
      remove: (name: string) => {
        setViews((current) => current.filter((named) => named.name !== name));
        setChosen(null);
      },
      reset: () => {
        apply(defaults);
        setChosen(null);
      },
    },
  };
}
