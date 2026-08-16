import type { JSX } from "@solidjs/web";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  onCleanup,
} from "solid-js";

import { focusAnchor, screenContribution } from "../app/screen-actions";
import { toHash } from "../routing/route";
import { clearVisited, visitedEntries } from "../store/visited";
import { EmptyState } from "../ui/states";
import {
  flatCommands,
  paletteGroups,
  type CommandAction,
  type CommandGroupName,
  type PaletteCommand,
} from "./commands";
import "./palette.css";

/**
 * §6 step 7's command palette: one input over three ranked groups, opened with
 * ⌘K / Ctrl-K.
 *
 * There are no direct keys anywhere else in the console, which makes this the
 * keyboard's only route to every navigation and every view action — the reason
 * screens contribute their controls (`app/screen-actions.ts`) rather than the
 * palette re-implementing them. The keys *inside* the palette are list
 * convention only: type, ↑↓, Enter, Esc. There is no keybinding column, because
 * there are no bindings to teach.
 *
 * Which rows stand and in what order is `commands.ts`'s; this file owns only the
 * interaction — opening, moving, activating, and giving focus back.
 */
export function CommandPalette(): JSX.Element {
  const uid = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  /**
   * The cursor, held as the *command's* id rather than as a place in the list.
   *
   * The list is live while the palette is up: `this screen` calls the mounted
   * screen's thunk, and a read settling there adds or removes anchors under an
   * open palette (a run that turns out to have failed grows a `failure` section).
   * A number would then quietly come to mean a different command and Enter would
   * run whichever one slid into its place — which is exactly what
   * `PaletteCommand.id` is documented to prevent, so this is the field that keeps
   * it. `null` means the cursor has not been moved, which resolves to the top.
   */
  const [activeId, setActiveId] = createSignal<string | null>(null);
  /**
   * The clock `seen 2m ago` is measured against, taken once per opening. Held
   * still while the palette is up so a row cannot re-word itself under the
   * cursor, and re-taken on the next opening so it is never stale.
   */
  const [openedAt, setOpenedAt] = createSignal(0);

  /*
   * Plain variables, not signals: neither is read reactively, and the focus one
   * is written from a native listener where a signal write would have no owner
   * to belong to. `restoreTo` is the element that had focus when the palette
   * opened — §6 step 7's focus restore — and is cleared by whoever consumes it.
   */
  let input: HTMLInputElement | undefined;
  let dialog: HTMLDivElement | undefined;
  let restoreTo: HTMLElement | null = null;

  /**
   * Only while it is up: `screenContribution()` calls the mounted screen's own
   * thunk, and a closed palette has no business re-running it on every read the
   * screen settles.
   */
  const groups = createMemo(() =>
    open()
      ? paletteGroups({
          query: query(),
          visited: visitedEntries(),
          contribution: screenContribution(),
          now: openedAt(),
        })
      : [],
  );
  const commands = createMemo(() => flatCommands(groups()));

  /**
   * Where that command stands in the list as it is *now*, resolved on read
   * rather than corrected on write: a write that tried to fix the cursor would
   * be deriving next state from a signal that has not flushed yet — the one
   * hazard this codebase keeps naming.
   *
   * A cursor whose command is gone from the list returns to the top, and not to
   * the row that took its place: the reader's choice is no longer on offer, and
   * pointing at its neighbour would claim it still is. -1 only when there is
   * nothing at all to point at, which is what leaves `aria-activedescendant`
   * unsaid rather than naming a row that is not in the document.
   */
  const activeIndex = (): number => {
    const list = commands();
    if (list.length === 0) {
      return -1;
    }
    const at = list.findIndex((command) => command.id === activeId());
    return at === -1 ? 0 : at;
  };
  const rowId = (index: number): string => `${uid}-row-${index}`;
  const activeRowId = (): string | undefined => {
    const at = activeIndex();
    return at < 0 ? undefined : rowId(at);
  };
  /**
   * The id of a group's heading, which is also the group's name — derived from
   * the name rather than from a counter, so it is the same id on every rebuild
   * of a list that is live while the palette is up.
   */
  const groupNameId = (name: CommandGroupName): string => `${uid}-${name.replace(/ /g, "-")}`;

  function openPalette(): void {
    restoreTo =
      document.activeElement instanceof HTMLElement && document.activeElement !== document.body
        ? document.activeElement
        : null;
    setQuery("");
    setActiveId(null);
    setOpenedAt(Date.now());
    setOpen(true);
  }

  function close(): void {
    setOpen(false);
  }

  function move(delta: number): void {
    const list = commands();
    if (list.length === 0) {
      return;
    }
    // Wraps, because a list this short has no bottom worth stopping at.
    const to = (activeIndex() + delta + list.length) % list.length;
    setActiveId(list[to].id);
  }

  /**
   * Put the reader at the head of the 88ch column (§1.4), which the shell owns
   * and which therefore survives the route change the screens inside it do not.
   * `app/app-shell.tsx` carries the `tabindex` that lets it take focus.
   *
   * Answers whether focus actually landed there, rather than assuming: a host
   * with no column — a palette mounted beside a bare screen in a test — has
   * nowhere to put the reader, and saying otherwise would drop the restore that
   * is the only thing left to give them.
   */
  function focusColumn(): boolean {
    const column = document.querySelector("main");
    column?.focus();
    return document.activeElement === column;
  }

  /**
   * Perform a command, answering whether it put focus somewhere itself.
   *
   * Two kinds do: an anchor moves the reader to a section, and a navigation
   * moves them into the column the next screen renders into. The close below
   * must then leave focus alone, or the reader would be dragged straight back to
   * whatever they were on before the palette opened — which for a navigation is
   * a control that no longer exists.
   */
  function perform(action: CommandAction): boolean {
    switch (action.kind) {
      case "navigate":
        window.location.hash = toHash(action.route);
        // Navigation unmounts the screen the reader was standing on, so the
        // control they had focus on before ⌘K is about to leave the document —
        // handing focus back to it would put the reader on <body>, one Tab from
        // the top of the page, after the console's *only* route to navigation.
        // The column is where the screen they asked for arrives, so that is
        // where they are put.
        return focusColumn();
      case "anchor":
        // False when the section is not on the page after all; then nothing took
        // focus and the reader is returned to where they were, rather than left
        // in an overlay that has just been removed.
        return focusAnchor(action.anchor);
      case "screen":
        action.run();
        return false;
      case "clear-visited":
        clearVisited();
        return false;
    }
  }

  function activate(command: PaletteCommand | undefined): void {
    if (command === undefined) {
      return;
    }
    // Performed before the close is staged, so an anchor's focus lands while the
    // overlay is still mounted and survives its removal.
    if (perform(command.action)) {
      restoreTo = null;
    }
    close();
  }

  /**
   * Every key the palette answers to, on the document rather than on the markup.
   *
   * ⌘K has to be heard wherever focus is, and hanging the rest here too keeps
   * ↑↓/Esc working when a reader has tabbed onto a row — and leaves the overlay
   * with no event handler on anything that is not a real control.
   */
  function onKeyDown(event: KeyboardEvent): void {
    if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
      // The browser has its own ⌘K. This is the console's only keyboard route to
      // navigation, so it takes the key outright rather than sharing it.
      event.preventDefault();
      if (open()) {
        close();
      } else {
        openPalette();
      }
      return;
    }
    if (!open()) {
      return;
    }
    switch (event.key) {
      case "Escape":
        event.preventDefault();
        close();
        return;
      case "ArrowDown":
        event.preventDefault();
        move(1);
        return;
      case "ArrowUp":
        event.preventDefault();
        move(-1);
        return;
      case "Enter":
        // A row that has been tabbed to activates itself on Enter; handling it
        // here as well would run the command twice.
        if (event.target === input) {
          event.preventDefault();
          activate(commands()[activeIndex()]);
        }
        return;
      case "Tab":
        wrapTab(event);
        return;
      default:
        return;
    }
  }

  /**
   * `aria-modal` claims nothing outside the palette is reachable, so Tab has to
   * make that true: otherwise the next Tab lands on a screen the reader can no
   * longer see, behind an overlay that is still up.
   */
  function wrapTab(event: KeyboardEvent): void {
    if (dialog === undefined) {
      return;
    }
    const stops = [...dialog.querySelectorAll<HTMLElement>("input, button")];
    if (stops.length === 0) {
      return;
    }
    const edge = event.shiftKey ? stops[0] : stops[stops.length - 1];
    const wrapTo = event.shiftKey ? stops[stops.length - 1] : stops[0];
    const at = document.activeElement;
    if (at === edge || !(at instanceof HTMLElement) || !dialog.contains(at)) {
      event.preventDefault();
      wrapTo.focus();
    }
  }

  document.addEventListener("keydown", onKeyDown);
  onCleanup(() => document.removeEventListener("keydown", onKeyDown));

  /*
   * Focus follows the overlay, in an effect rather than in the ref: a ref runs
   * while the element is still detached (proven here — an element that is not in
   * the document cannot take focus), and the effect runs once the DOM has caught
   * up with the signal. Two functions because that is the only shape Solid 2's
   * `createEffect` has.
   */
  createEffect(
    () => open(),
    (isOpen) => {
      if (isOpen) {
        input?.focus();
        return;
      }
      const target = restoreTo;
      restoreTo = null;
      target?.focus();
    },
  );

  /*
   * The list scrolls, so ↑↓ have to bring the cursor with them — a row the
   * reader cannot see is a cursor that has stopped meaning anything.
   *
   * `scrollIntoView` is called defensively for the reason `focusAnchor` calls it
   * that way: jsdom does not implement it, and an unguarded call would make
   * every keyboard test a test of the environment instead of the behaviour.
   */
  createEffect(
    () => activeRowId(),
    (id) => {
      if (id !== undefined) {
        document.getElementById(id)?.scrollIntoView?.({ block: "nearest" });
      }
    },
  );

  return (
    <Show when={open()}>
      <div class="palette-scrim">
        <div
          class="palette"
          role="dialog"
          aria-modal="true"
          aria-label="command palette"
          ref={(element) => (dialog = element)}
        >
          <input
            ref={(element) => (input = element)}
            class="palette-input"
            type="text"
            autocomplete="off"
            aria-label="command or id"
            placeholder="type an id, a section, or a command"
            /*
             * A textbox that points at its active row, and deliberately not a
             * `combobox`: a combobox has to control a listbox, and a listbox's
             * options are not tab stops — while every row here is a real button
             * a keyboard reader can reach. Claiming the pattern without keeping
             * it is worse for a reader than not claiming it.
             */
            aria-controls={`${uid}-list`}
            aria-activedescendant={activeRowId()}
            value={query()}
            onInput={(event) => {
              setQuery(event.currentTarget.value);
              // A new query is a new list; the cursor starts at its top.
              setActiveId(null);
            }}
          />
          <div class="palette-groups" id={`${uid}-list`}>
            <For each={groups()}>
              {(group) => (
                /*
                 * §6 step 7's ranking, said rather than only drawn: a named
                 * `group` a reader is announced into and out of, holding a
                 * heading, a real list of its rows, and whatever the group has
                 * to say about what it has not got.
                 *
                 * A grouped list of buttons, and deliberately not a
                 * `listbox`/`option` pair, for the reason the input gives above
                 * for not being a `combobox`: an `option` is not a tab stop and
                 * every row here is a button a keyboard reader can reach and
                 * press. Half a listbox — options that are buttons, or a
                 * listbox with no combobox controlling it — would be a worse
                 * claim than the plain one this makes. The list is what carries
                 * the ranking: `1 of 3` inside `go to` is the step the reader
                 * would otherwise lose.
                 */
                <div class="palette-group" role="group" aria-labelledby={groupNameId(group.name)}>
                  {/* A heading, at `SectionLabel`'s level: a group name opens a
                      region of the overlay exactly as a section label opens one
                      of a screen, and the dialog's own name is above both. */}
                  <h2 class="palette-group-name label" id={groupNameId(group.name)}>
                    {group.name}
                  </h2>
                  {/* `role="list"` beside the `<ul>`: the rows are drawn with no
                      markers, and a list styled that way loses its list semantics
                      in Safari — the same spelling `json-view` and
                      `draft-structure` use. */}
                  <Show when={group.commands.length > 0}>
                    <ul class="palette-rows" role="list">
                      <For each={group.commands}>
                        {(command) => (
                          <li>
                            <button
                              type="button"
                              class="palette-row"
                              id={rowId(command.index)}
                              // The row's own text is its name — there is no aria-label
                              // to drift from what a reader can see and say.
                              aria-current={command.index === activeIndex() ? "true" : "false"}
                              onClick={() => activate(command)}
                            >
                              {/* The cursor is a glyph as well as a tint, so the
                                  active row is never told by colour alone. */}
                              <span class="palette-row-mark" aria-hidden="true">
                                {command.index === activeIndex() ? "▸" : ""}
                              </span>
                              <span class="palette-row-text">{command.text}</span>
                              <span class="palette-row-detail frame">{command.detail}</span>
                            </button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                  {/* A group with genuinely no rows is a region that is empty,
                      which is the one thing `EmptyState` states — in words,
                      because a blank is a claim. */}
                  <Show when={group.commands.length === 0 ? group.note : null}>
                    {(note) => (
                      <div class="palette-group-note">
                        <EmptyState region={group.name} reason={note()} />
                      </div>
                    )}
                  </Show>
                  {/* A note *beside* rows is not an emptiness: `go to` explains
                      why the typed text does not also address a draft while it
                      is offering rows that do address something. Drawing the
                      region-is-empty primitive over that announces the opposite
                      of what a reader can see, so it is rendered as what it is. */}
                  <Show when={group.commands.length > 0 ? group.note : null}>
                    {(note) => (
                      <p class="palette-note frame" role="note">
                        {note()}
                      </p>
                    )}
                  </Show>
                </div>
              )}
            </For>
          </div>
        </div>
      </div>
    </Show>
  );
}
