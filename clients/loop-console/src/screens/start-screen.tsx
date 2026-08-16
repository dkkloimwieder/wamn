import type { JSX } from "@solidjs/web";
import { createSignal, For, Show } from "solid-js";

import { readStatus, readStatusWord } from "../app/read-status";
import { proxyTarget } from "../config";
import { draftAddress } from "../palette/commands";
import { fixtureIndex, usingFixtures, type IndexEntry } from "../reader/fixture-index";
import { MAX_ID_LENGTH, toHash, type Route } from "../routing/route";
import { visitedEntries } from "../store/visited";
import "./start-screen.css";

/**
 * §2.1 — the only screen with no verdict. Its job is to get the author to one,
 * so it is a single calm action: where the console is pointed, whether the last
 * read landed, and one field to paste an id into.
 *
 * Visited entities deliberately do not appear here. §2.1 says they live in the
 * side panel (§2.0) and are not repeated, so this screen never grows into a
 * second, differently-ranked recents list.
 */

/** The three kinds a pasted id can address. §2.1 draws them `run · report · draft`. */
const kinds = ["run", "report", "draft"] as const;
type Kind = (typeof kinds)[number];

/**
 * Whether the typed text could be a route segment at all — the same rule the
 * palette applies, because the two fields accept the same thing and an author
 * who learns one has learned the other.
 */
function looksLikeId(text: string): boolean {
  return text.length > 0 && text.length <= MAX_ID_LENGTH && !/[\s/#]/.test(text);
}

/**
 * §2.1's "kind auto-inferred from recents".
 *
 * A run id and a report id are indistinguishable — the console is told nothing
 * about their format — so the only evidence about which one an author means is
 * which kind that author has been reading. With nothing visited there is no
 * evidence at all, which is what §2.1's "radio fallback when unknown" is for.
 */
export function inferredKind(visited: ReadonlyArray<{ readonly kind: string }>): Kind | null {
  const first = visited.find((entry) => (kinds as readonly string[]).includes(entry.kind));
  return first === undefined ? null : (first.kind as Kind);
}

/**
 * The route a typed jump addresses, or null.
 *
 * `id@revision` is the one shape that can only mean a draft, so it wins over the
 * selected kind rather than being overridden by it — an author who typed a
 * revision has said what they meant. A bare id under the `draft` kind addresses
 * nothing, because a revision may not be guessed: revision 0 is a real revision,
 * so a default would silently open a different draft than the one asked for.
 */
export function jumpRoute(text: string, kind: Kind): Route | null {
  const query = text.trim();
  const draft = draftAddress(query);
  if (draft !== null) {
    return { kind: "draft", id: draft.id, revision: draft.revision };
  }
  if (!looksLikeId(query) || kind === "draft") {
    return null;
  }
  return { kind, id: query };
}

/** Why a jump did not go anywhere, in the rule it actually broke. */
function refusal(text: string, kind: Kind): string | null {
  const query = text.trim();
  if (query === "") {
    return null;
  }
  if (jumpRoute(query, kind) !== null) {
    return null;
  }
  if (kind === "draft") {
    return "a draft is addressed by an id and a revision, so a bare id cannot name one — type id@revision";
  }
  return "that is not an id a route can carry — ids are one segment, with no spaces, slashes or hashes";
}

export function StartScreen(): JSX.Element {
  const [text, setText] = createSignal("");
  // Seeded from recents, then the author's own. Once they have chosen, the
  // choice stands: re-inferring under them would move the target between the
  // moment they read the row and the moment they press Enter.
  const [chosen, setChosen] = createSignal<Kind | null>(null);
  const kind = (): Kind => chosen() ?? inferredKind(visitedEntries()) ?? "run";
  const nothingVisited = () => visitedEntries().length === 0;

  function go(event: Event): void {
    event.preventDefault();
    const route = jumpRoute(text(), kind());
    if (route !== null) {
      window.location.hash = toHash(route);
    }
  }

  return (
    <section class="start-screen">
      <h1 class="start-name">wamn loop console</h1>
      <p class="start-target frame">dev target: {proxyTarget}</p>
      <p class="start-status frame">status: {readStatusWord(readStatus())}</p>

      {/* A form, so Enter navigates without a key handler of its own — §2.6
          leaves the console no direct keys, and a submit is not one. */}
      <form class="start-jump" onSubmit={go}>
        <label class="visually-hidden" for="start-jump-field">
          paste a run, report or draft id
        </label>
        <input
          id="start-jump-field"
          class="start-jump-field"
          type="text"
          autocomplete="off"
          spellcheck={false}
          placeholder="paste a run, report, or draft id…"
          value={text()}
          // §2.1: focused on load. The element exists when the ref runs but is
          // not yet in the document, and focus on a detached node does nothing.
          ref={(element: HTMLInputElement) => queueMicrotask(() => element.focus())}
          onInput={(event) => setText((event.currentTarget as HTMLInputElement).value)}
        />
      </form>

      {/*
       * §2.1 shows the three kinds and calls the radios a fallback for when the
       * kind cannot be inferred. They are drawn always: the inference is a guess
       * from history, and a guess an author cannot see is one they cannot
       * correct — they would paste a report id, land on a missing run, and have
       * nothing on the screen to tell them why.
       */}
      <fieldset class="start-kinds">
        <legend class="visually-hidden">which kind of id</legend>
        <For each={kinds}>
          {(candidate) => (
            <label class="start-kind">
              <input
                type="radio"
                name="start-kind"
                value={candidate}
                checked={kind() === candidate}
                onChange={() => setChosen(candidate)}
              />
              <span>{candidate}</span>
            </label>
          )}
        </For>
      </fieldset>

      <Show when={refusal(text(), kind())}>
        {(reason) => <p class="start-refusal frame">{reason()}</p>}
      </Show>

      {/*
       * §2.1's first-run empty state, in the spec's own words — but only where
       * it is true. Against fixtures there IS something, and telling an author
       * to go and run a draft with the CLI would be the console stating an
       * absence it is itself holding the contents of.
       */}
      <Show when={nothingVisited() && !usingFixtures()}>
        <p class="start-empty frame">
          Nothing yet — run a draft with the CLI, then paste its id.
        </p>
      </Show>

      <Show when={usingFixtures()}>
        <FixtureIndex />
      </Show>
    </section>
  );
}

/**
 * The dev index (see `reader/fixture-index.ts`).
 *
 * §2.0 is explicit that the console has no list endpoints and that the panel's
 * tree is what this browser has visited — so this is not that list arriving. It
 * is the fixture set naming itself, and it is gone the moment the reader is the
 * real one, which is also when a platform id comes from the CLI that printed it.
 */
function FixtureIndex(): JSX.Element {
  const index = fixtureIndex();
  const groups = [
    { name: "runs", entries: index.runs },
    { name: "reports", entries: index.reports },
    { name: "drafts", entries: index.drafts },
  ];

  return (
    <section class="start-fixtures">
      <p class="start-fixtures-note frame">
        reading fixtures — these are the objects this build ships. The platform
        offers no listing; the side panel keeps what you visit.
      </p>
      <For each={groups}>
        {(group) => (
          <div class="start-fixture-group">
            <h2 class="start-fixture-kind label">{group.name}</h2>
            <ul class="start-fixture-rows" role="list">
              <For each={group.entries}>
                {(entry) => (
                  <li>
                    <FixtureRow entry={entry} />
                  </li>
                )}
              </For>
            </ul>
          </div>
        )}
      </For>
    </section>
  );
}

function FixtureRow(props: { entry: IndexEntry }): JSX.Element {
  return (
    <a class="start-fixture-row" href={toHash(props.entry.route)}>
      <span class="start-fixture-address">{props.entry.address}</span>
      <span class="start-fixture-note frame">{props.entry.note}</span>
    </a>
  );
}
