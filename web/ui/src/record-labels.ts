/**
 * The text of the records a table names by key, read once for each key.
 *
 * A table column holds a record key. The generated table passes the read that
 * returns one record's text, and each cell asks for its own key. The first ask
 * starts the read, and every later ask for that key reads the kept answer, so
 * a page that names one record in many rows reads it once.
 *
 * A write can rename a record, so after each write on the transport every key
 * asked so far reads again (wamn-p398). A cell keeps its old text until the
 * new answer comes. Every write counts, as it does for a stored read, until
 * write contracts name their models (wamn-fjdo).
 *
 * A cell shows nothing while its first read runs. A read that returns no text
 * shows the key, so a cell never hides which record it names.
 */

import { createSignal, onCleanup } from "solid-js";

import { afterWrites, type Transport } from "@wamn/web-runtime";

/** The text one record key shows, or null when its read returned none. */
type Answer = string | null;

export function createRecordLabels(
  transport: Transport,
  read: (key: string) => Promise<string | null>,
): (key: string | null | undefined) => string {
  const [answers, setAnswers] = createSignal<ReadonlyMap<string, Answer>>(new Map());
  /** Each key asked so far, with the number of its latest read. */
  const asked = new Map<string, number>();
  let reads = 0;
  const ask = (key: string) => {
    const mine = ++reads;
    asked.set(key, mine);
    // Only the latest read of a key answers, so an older one that ends late
    // cannot put back an old name.
    const answer = (text: Answer) => {
      if (asked.get(key) === mine) {
        setAnswers((current) => new Map(current).set(key, text === "" ? null : text));
      }
    };
    read(key).then(answer, () => answer(null));
  };
  onCleanup(
    afterWrites(transport, () => {
      for (const key of [...asked.keys()]) {
        ask(key);
      }
    }),
  );
  return (key) => {
    if (key === null || key === undefined || key === "") {
      return "";
    }
    const known = answers().get(key);
    if (known !== undefined) {
      return known ?? key;
    }
    if (!asked.has(key)) {
      ask(key);
    }
    return "";
  };
}
