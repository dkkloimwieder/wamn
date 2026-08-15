import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";

import type { NotFoundLiteral, ReaderError, RefusalLiteral } from "../reader/errors";
import { KeyValue } from "./key-value";
import type { Tone } from "./status";
import "./error-panel.css";
import "./tone.css";

/**
 * The typed reader errors, with the platform's literals verbatim.
 *
 * The three states must not blur into one "error": a not-found is a 2xx refusal
 * — the platform answered and holds nothing — a refusal is the platform
 * declining a read it understood, and a network failure means no HTTP response
 * came back at all, so nothing is known either way. They get three words, three
 * tones, and three explanations, and the literal itself is never rewritten:
 * `run-not-found` is what the author will grep for in the platform's source.
 */

type Face = {
  readonly tone: Tone;
  /** The console's own word for the state, in the frame face. */
  readonly word: string;
  /** What the platform said: a refusal literal, or the transport's message. */
  readonly said: string;
  readonly explanation: string;
};

const notFoundExplanation: Record<NotFoundLiteral, string> = {
  "run-not-found": "the platform answered, and holds no run under this id.",
  "report-not-found": "the platform answered, and holds no report under this id.",
  "draft-revision-not-found": "the platform answered, and holds no draft at this revision.",
};

const refusalExplanation: Record<RefusalLiteral, string> = {
  "authorization-denied":
    "the platform understood the read and refused it: the credentials presented are not authorized for it.",
  "unsupported-contract-version":
    "the platform understood the read and refused it: it does not speak the contract version this console asked for.",
};

function face(error: ReaderError): Face {
  switch (error.kind) {
    case "not-found":
      return {
        tone: "neutral",
        word: "not found",
        said: error.literal,
        explanation: notFoundExplanation[error.literal],
      };
    case "refusal":
      return {
        tone: "warn",
        word: "refused",
        said: error.literal,
        explanation: refusalExplanation[error.literal],
      };
    case "network":
      return {
        tone: "fail",
        word: "no response",
        said: error.message,
        explanation:
          "no HTTP response came back, so whether the platform ever saw this read is unknown.",
      };
  }
}

export function ErrorPanel(props: { error: ReaderError }): JSX.Element {
  const shown = () => face(props.error);
  const missing = () => (props.error.kind === "not-found" ? props.error : null);
  /** A refusal carries no resource or id, so its detail is all there is to name. */
  const detail = () => (props.error.kind === "refusal" ? props.error.detail : null);

  return (
    <section class="error-panel" data-tone={shown().tone}>
      <p class="error-panel-word label">{shown().word}</p>
      <p class="error-panel-said">{shown().said}</p>
      <p class="error-panel-explanation frame">{shown().explanation}</p>
      <Show when={missing()}>
        {(error) => (
          <div class="error-panel-asked">
            <KeyValue label="resource">{error().resource}</KeyValue>
            <KeyValue label="id">{error().id}</KeyValue>
            <Show when={error().revision !== null}>
              <KeyValue label="revision">{error().revision}</KeyValue>
            </Show>
          </div>
        )}
      </Show>
      <Show when={detail() !== null}>
        <div class="error-panel-asked">
          <KeyValue label="detail">{detail()}</KeyValue>
        </div>
      </Show>
    </section>
  );
}
