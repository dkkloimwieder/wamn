/**
 * The text of the records a table names by key, read once for each key.
 *
 * A table column holds a record key. The generated table passes the read that
 * returns one record's text, and each cell asks for its own key. The first ask
 * starts the read, and every later ask for that key reads the kept answer, so
 * a page that names one record in many rows reads it once.
 *
 * A cell shows nothing while its read runs. A read that returns no text shows
 * the key, so a cell never hides which record it names.
 */

import { createSignal } from "solid-js";

/** The text one record key shows, or null when its read returned none. */
type Answer = string | null;

export function createRecordLabels(
  read: (key: string) => Promise<string | null>,
): (key: string | null | undefined) => string {
  const [answers, setAnswers] = createSignal<ReadonlyMap<string, Answer>>(new Map());
  const asked = new Set<string>();
  const answer = (key: string, text: Answer) =>
    setAnswers((current) => new Map(current).set(key, text === "" ? null : text));
  return (key) => {
    if (key === null || key === undefined || key === "") {
      return "";
    }
    const known = answers().get(key);
    if (known !== undefined) {
      return known ?? key;
    }
    if (!asked.has(key)) {
      asked.add(key);
      read(key).then(
        (text) => answer(key, text),
        () => answer(key, null),
      );
    }
    return "";
  };
}
