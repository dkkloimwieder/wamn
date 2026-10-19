import { createSignal, For, onCleanup, Show, type Component } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Field, FieldError, Input, Label } from "@wfe/components/fields";
import { createForm, type FormContext } from "@wfe/form";
import {
  enroll,
  environments,
  recover,
  resetPassword,
  signIn,
  type Environment,
  type SessionOptions,
} from "@wamn/web-runtime";
import type { Address } from "./address";
import type { Scope } from "./scope";
import { Link, Page } from "./page";

interface IdentityProps {
  readonly title: string;
  readonly options: SessionOptions;
  readonly address: Address;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function signInFailed(error: unknown): string {
  return `Sign in failed: ${message(error)}.`;
}

/** The same form serves the root and a pasted screen address. */
export function SignInForm(props: {
  readonly submit: (email: string, password: string, context: FormContext) => Promise<void>;
  readonly trouble: string | null;
  readonly address: Address;
}): JSX.Element {
  const defaults = { email: "", password: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: () => ({}),
    onSubmit: async (snapshot, context) => {
      await props.submit(snapshot.email, snapshot.password, context);
      return { ok: true };
    },
  });
  const trouble = () => (form.phase === "error" ? signInFailed(form.failure) : props.trouble);
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void form.submit();
      }}
    >
      <Field>
        <Label for="shell-email">email</Label>
        <Input
          id="shell-email"
          type="email"
          autocomplete="username"
          value={form.values.email}
          onValueChange={(value) => form.setValue("email", value)}
        />
      </Field>
      <Field>
        <Label for="shell-password">password</Label>
        <Input
          id="shell-password"
          type="password"
          autocomplete="current-password"
          value={form.values.password}
          onValueChange={(value) => form.setValue("password", value)}
        />
      </Field>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Button type="submit" disabled={form.pending}>
        sign in
      </Button>
      <Link address={props.address} href="/recover">
        forgot password
      </Link>
    </form>
  );
}

/** Identity names the reachable audiences; the session reads their permissions. */
function ChooseEnvironment(props: IdentityProps & { scope: Scope }): JSX.Element {
  const [credentials, setCredentials] = createSignal<{ email: string; password: string } | null>(null);
  const [reachable, setReachable] = createSignal<readonly Environment[]>([]);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const [entering, setEntering] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const enter = async (email: string, password: string, aud: string, signal?: AbortSignal) => {
    setTrouble(null);
    setEntering(true);
    try {
      await signIn(email, password, aud, props.options);
      if (active && !signal?.aborted) props.address.navigate(`/${aud}`);
    } finally {
      if (active) setEntering(false);
    }
  };
  return (
    <Show
      when={credentials() === null || reachable().length > 0}
      fallback={<Page title="No access has been granted." />}
    >
      <Page title={props.title}>
        <SignInForm
          address={props.address}
          trouble={null}
          submit={async (email, password, context) => {
            const found = await environments(email, password, props.scope, props.options);
            if (context.signal.aborted) return;
            setReachable(found);
            setCredentials({ email, password });
            const only = found.length === 1 ? found[0] : undefined;
            if (only) await enter(email, password, only.aud, context.signal);
          }}
        />
        <Show when={credentials()}>
          {(held) => (
            <>
              <For each={reachable().length > 1 ? reachable() : []}>
                {(environment) => (
                  <Button
                    disabled={entering()}
                    onClick={() =>
                      void enter(held().email, held().password, environment.aud).catch((error: unknown) => {
                        if (active) setTrouble(signInFailed(error));
                      })
                    }
                  >
                    {environment.project === undefined
                      ? "Control"
                      : `${environment.org}/${environment.project}/${environment.env ?? ""}`}
                  </Button>
                )}
              </For>
              <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
            </>
          )}
        </Show>
      </Page>
    </Show>
  );
}

/** Invitation and reset secrets stay in the fragment until a password POST. */
function NewPassword(
  props: IdentityProps & {
    readonly id: string;
    readonly missing: string | null;
    readonly failed: string;
    readonly submit: (email: string, password: string) => Promise<void>;
  },
): JSX.Element {
  const defaults = { email: "", password: "", again: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) => {
      const error = props.missing ?? (snapshot.password !== snapshot.again ? "The two passwords differ." : null);
      return error ? { form: error } : {};
    },
    onSubmit: async (snapshot, context) => {
      await props.submit(snapshot.email, snapshot.password);
      if (!context.signal.aborted) props.address.navigate("/", true);
      return { ok: true };
    },
  });
  const trouble = () =>
    props.missing ?? form.errors.form ?? (form.phase === "error" ? `${props.failed}: ${message(form.failure)}.` : null);
  return (
    <Page title={props.title}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void form.submit();
        }}
      >
        <Field>
          <Label for={`${props.id}-email`}>email</Label>
          <Input
            id={`${props.id}-email`}
            type="email"
            autocomplete="username"
            value={form.values.email}
            onValueChange={(value) => form.setValue("email", value)}
          />
        </Field>
        <Field>
          <Label for={`${props.id}-password`}>password</Label>
          <Input
            id={`${props.id}-password`}
            type="password"
            autocomplete="new-password"
            value={form.values.password}
            onValueChange={(value) => form.setValue("password", value)}
          />
        </Field>
        <Field>
          <Label for={`${props.id}-again`}>password again</Label>
          <Input
            id={`${props.id}-again`}
            type="password"
            autocomplete="new-password"
            value={form.values.again}
            onValueChange={(value) => form.setValue("again", value)}
          />
        </Field>
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Button type="submit" disabled={form.pending}>
          set password
        </Button>
      </form>
    </Page>
  );
}

function AcceptInvitation(props: IdentityProps): JSX.Element {
  const code = window.location.hash.slice(1);
  const split = code.indexOf(":");
  return (
    <NewPassword
      {...props}
      id="invite"
      missing={split > 0 ? null : "This address holds no invitation code. Open the link in the invitation mail."}
      failed="Sign up failed"
      submit={(_, password) => enroll(code.slice(0, split), code.slice(split + 1), password, props.options)}
    />
  );
}

function ResetPassword(props: IdentityProps): JSX.Element {
  const secret = window.location.hash.slice(1);
  return (
    <NewPassword
      {...props}
      id="reset"
      missing={secret === "" ? "This address holds no reset code. Open the link in the reset mail." : null}
      failed="Reset failed"
      submit={(email, password) => resetPassword(email, secret, password, props.options)}
    />
  );
}

function RecoverPassword(props: IdentityProps): JSX.Element {
  const defaults = { email: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: () => ({}),
    onSubmit: async (snapshot) => {
      await recover(snapshot.email, props.options);
      return { ok: true };
    },
  });
  return (
    <Page title={props.title}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void form.submit();
        }}
      >
        <Field>
          <Label for="recover-email">email</Label>
          <Input
            id="recover-email"
            type="email"
            autocomplete="username"
            value={form.values.email}
            onValueChange={(value) => form.setValue("email", value)}
          />
        </Field>
        <Show when={form.phase === "error"}>
          <FieldError role="alert">Recovery failed: {message(form.failure)}.</FieldError>
        </Show>
        <Show when={form.phase === "success"}>
          <p>If this address has an account, a mail with a reset link is on its way.</p>
        </Show>
        <Button type="submit" disabled={form.pending}>
          send reset link
        </Button>
        <Link address={props.address} href="/">
          sign in
        </Link>
      </form>
    </Page>
  );
}

const pages = new Map<string, Component<IdentityProps & { scope: Scope }>>([
  ["", ChooseEnvironment],
  ["/invite", AcceptInvitation],
  ["/recover", RecoverPassword],
  ["/reset", ResetPassword],
]);

/** The identity pages own their address table. */
export function identityPage(pathname: string): Component<IdentityProps & { scope: Scope }> | undefined {
  return pages.get(pathname.replace(/\/$/, ""));
}
