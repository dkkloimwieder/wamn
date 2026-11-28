import { createSignal, Show, type Component } from "solid-js";
import type { JSX } from "@solidjs/web";
import { FieldError, Input } from "@wfe/components/fields";
import { createForm, type FormContext } from "@wfe/form";
import { FormView, type CustomFieldProps, type FormDescription } from "@wfe/generator";
import { emailError } from "./email";
import { enroll, recover, resetPassword, type SessionOptions } from "@wamn/web-runtime";
import type { Address } from "./address";
import { EntryPage, NavigationLink } from "./page";

interface IdentityProps {
  readonly options: SessionOptions;
  readonly address: Address;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function signInFailed(error: unknown): string {
  return `Sign in failed: ${message(error)}.`;
}

type Credentials = { email: string; password: string };
const Email = (props: CustomFieldProps<{ email: string }, "email">) => (
  <Input {...props} type="email" autocomplete="username" />
);
const Password = (props: CustomFieldProps<Credentials, "password">) => (
  <Input {...props} type="password" autocomplete="current-password" />
);
const NewPasswordInput = (props: CustomFieldProps<{ password: string; again: string }, "password" | "again">) => (
  <Input {...props} type="password" autocomplete="new-password" />
);

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
    validate: (snapshot) => ({ fields: { email: emailError(snapshot.email) } }),
    onSubmit: async (snapshot, context) => {
      try {
        await props.submit(snapshot.email, snapshot.password, context);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: signInFailed(error) } };
      }
    },
  });
  const description = {
    id: "sign-in",
    fields: [
      { field: "email", label: "Email", control: "custom" },
      { field: "password", label: "Password", control: "custom" },
    ],
    submit: { id: "sign-in", label: "Sign in" },
  } as const satisfies FormDescription<Credentials>;
  return (
    <>
      <FormView
        description={description}
        bindings={{
          form,
          fields: { email: Email, password: Password },
        }}
      />
      <NavigationLink href="/recover" navigate={props.address.navigate}>
        Forgot password
      </NavigationLink>
      <Show when={!form.errors.form && props.trouble}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
    </>
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
      const email = emailError(snapshot.email);
      if (email) return { fields: { email } };
      const error = props.missing ?? (snapshot.password !== snapshot.again ? "The two passwords differ." : null);
      return error ? { form: error } : {};
    },
    onSubmit: async (snapshot, context) => {
      try {
        await props.submit(snapshot.email, snapshot.password);
        if (!context.signal.aborted) props.address.navigate("/", true);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: `${props.failed}: ${message(error)}.` } };
      }
    },
  });
  const description = {
    id: props.id,
    fields: [
      { field: "email", label: "Email", control: "custom" },
      { field: "password", label: "Password", control: "custom" },
      { field: "again", label: "Password again", control: "custom" },
    ],
    submit: { id: "set-password", label: "Set password" },
  } as const satisfies FormDescription<typeof defaults>;
  return (
    <EntryPage title={props.id === "invite" ? "Accept invitation" : "Reset password"}>
      <Show when={props.missing && !form.errors.form}>
        <FieldError role="alert">{props.missing}</FieldError>
      </Show>
      <FormView
        description={description}
        bindings={{
          form,
          fields: { email: Email, password: NewPasswordInput, again: NewPasswordInput },
        }}
      />
    </EntryPage>
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
    validate: (snapshot) => ({ fields: { email: emailError(snapshot.email) } }),
    onSubmit: async (snapshot) => {
      try {
        await recover(snapshot.email, props.options);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: `Recovery failed: ${message(error)}.` } };
      }
    },
  });
  const description = {
    id: "recover",
    fields: [{ field: "email", label: "Email", control: "custom" }],
    submit: { id: "recover", label: "Send reset link" },
  } as const satisfies FormDescription<typeof defaults>;
  return (
    <EntryPage title="Recover password">
      <FormView
        description={description}
        bindings={{
          form,
          fields: { email: Email },
        }}
      />
      <Show when={form.phase === "success"}>
        <p>If this address has an account, a mail with a reset link is on its way.</p>
      </Show>
      <NavigationLink href="/" navigate={props.address.navigate}>
        Sign in
      </NavigationLink>
    </EntryPage>
  );
}

const pages = new Map<string, Component<IdentityProps>>([
  ["/invite", AcceptInvitation],
  ["/recover", RecoverPassword],
  ["/reset", ResetPassword],
]);

/** The identity pages own their address table. */
export function identityPage(pathname: string): Component<IdentityProps> | undefined {
  return pages.get(pathname.replace(/\/$/, ""));
}
