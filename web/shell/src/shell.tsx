/**
 * The app shell: sign in, the session, the layout and the routes.
 *
 * The first segment of every address is the environment, as its audience, so
 * a reload renews the session with nothing in browser storage. Each screen
 * route sits below that segment and renders inside the layout. The shell owns
 * no screen: the application writes each one from its generated components.
 * The shell owns the router, so a screen gets its address values and a way to
 * open another address as props, and never imports the router.
 */

import {
  A,
  Navigate,
  Route,
  Router,
  useLocation,
  useNavigate,
  useParams,
  type RouteSectionProps,
} from "@solidjs/router";
import {
  createContext,
  createSignal,
  For,
  lazy,
  Match,
  onCleanup,
  Show,
  Suspense,
  Switch,
  useContext,
  type Component,
  type JSX,
} from "solid-js";

import { Button, CardPage, Field, FieldError, FieldGroup, FieldLabel, Input, ScreenActions } from "@wamn/ui";
import {
  createTransport,
  enroll,
  environments,
  keepSession,
  recover,
  resetPassword,
  signIn,
  type Environment,
  type SessionOptions,
  type SessionState,
  type Transport,
} from "@wamn/web-runtime";

/**
 * The path every API call goes under. The proxy in front of the page strips
 * it before the release sees the call, so no page path meets a route template.
 */
export const API_BASE = "/api";

/** What the shell hands every screen. */
export interface ScreenProps {
  /** The transport of the signed-in session. */
  readonly transport: Transport;
  /** The values in the address, by the names in the route path, such as `id` for `pallets/:id`. */
  readonly params: Readonly<Record<string, string | undefined>>;
  /** The values in the query of the address, such as `palletId` for `inventory/move?palletId=<id>`. */
  readonly search: Readonly<Record<string, string | undefined>>;
  /**
   * Opens a path below the environment, such as `pallets/<id>` or
   * `inventory/move?palletId=<id>`. The caller encodes each value it puts in
   * the path.
   */
  readonly open: (path: string) => void;
  /**
   * Returns to the page that opened this one. When no page of the application
   * opened it, for example a pasted address, it goes to the first screen of
   * the section.
   */
  readonly close: () => void;
}

/** One button above a route, which opens a path. */
export interface ShellAction {
  /** The button label, from the form label the generated module exports. */
  readonly label: string;
  /**
   * The path it opens, below the environment. A parameter of the route, such
   * as `:id` in `locations/:id/update`, takes its value from the address.
   */
  readonly path: string;
}

/** One route of the application, with no navigation entry, such as a record page or a form. */
export interface ShellRoute {
  /** The path below the environment, with no leading slash, such as `pallets/:id`. */
  readonly path: string;
  readonly component: Component<ScreenProps>;
  /** The buttons above the route, such as the create form of a table. */
  readonly actions?: readonly ShellAction[];
}

/** One screen of the application, on its own route, with a navigation entry. */
export interface ShellScreen extends ShellRoute {
  /** The navigation label, from the screen label the generated module exports. */
  readonly label: string;
}

/**
 * The screens of one model. The navigation names a model with one screen by
 * the model alone, and lists the screen labels below the model only when it
 * has more than one screen.
 */
export interface ShellSection {
  /** The model name, such as `Pallets`. */
  readonly label: string;
  readonly screens: readonly ShellScreen[];
  /** The routes of the model that the navigation does not list, such as its record page and its forms. */
  readonly routes?: readonly ShellRoute[];
}

export interface ShellProps {
  /** The application name, in the sidebar and on the sign in card. */
  readonly title: string;
  /** The org and the project the application signs in to. No other environment is offered or accepted. */
  readonly org: string;
  readonly project: string;
  readonly sections: readonly ShellSection[];
  /** The fetch every call uses. The global one is the default. */
  readonly fetch?: typeof globalThis.fetch;
}

const TransportContext = createContext<Transport>();

/** The page of a signed-in session. It loads after sign in, so the sign in page stays small. */
const Layout = lazy(() => import("./layout"));

/** The state a page opened by `open` carries, so that `close` can go back to it. */
interface Opened {
  readonly opened: true;
}

/**
 * One route with the transport of the session it renders in, its address
 * values, a way to open another path and a way back. Each one is read here,
 * while the route renders. A component uses its props later, in an event
 * handler, where no context is in reach.
 */
function screenRoute(route: ShellRoute, home: string): Component {
  return () => {
    const transport = useContext(TransportContext);
    if (transport === undefined) {
      throw new Error("a screen renders outside a signed-in session");
    }
    const params = useParams();
    const location = useLocation<Opened>();
    const navigate = useNavigate();
    const open = (path: string) => navigate(`/${params.aud}/${path}`, { state: { opened: true } });
    const close = () =>
      location.state?.opened === true ? navigate(-1) : navigate(`/${params.aud}/${home}`, { replace: true });
    const fill = (path: string) => path.replace(/:(\w+)/g, (_, name: string) => encodeURIComponent(params[name] ?? ""));
    return (
      <>
        <Show when={route.actions}>
          {(actions) => (
            <ScreenActions>
              <For each={actions()}>
                {(action) => (
                  <Button variant="outline" size="sm" onClick={() => open(fill(action.path))}>
                    {action.label}
                  </Button>
                )}
              </For>
            </ScreenActions>
          )}
        </Show>
        <route.component
          transport={transport}
          params={params}
          search={single(location.query)}
          open={open}
          close={close}
        />
      </>
    );
  };
}

/** The query values, with the first value of a repeated name. */
function single(query: Readonly<Record<string, string | string[] | undefined>>): Record<string, string | undefined> {
  return Object.fromEntries(
    Object.entries(query).map(([name, value]) => [name, Array.isArray(value) ? value[0] : value]),
  );
}

/**
 * A screen whose component loads with its module. The route table names the
 * module and picks the component from it, so a screen loads its generated
 * component, and the table or form code it renders, only when it opens.
 */
export function screen<M>(load: () => Promise<M>, pick: (module: M) => Component<ScreenProps>): Component<ScreenProps> {
  return lazy(async () => ({ default: pick(await load()) }));
}

export function Shell(props: ShellProps): JSX.Element {
  const options: SessionOptions = props.fetch === undefined ? {} : { fetch: props.fetch };
  const screens = props.sections.flatMap((section) => section.screens);
  const routes = props.sections.flatMap((section) =>
    [...section.screens, ...(section.routes ?? [])].map((route) => ({ route, home: section.screens[0]?.path ?? "" })),
  );
  const first = screens[0];
  return (
    // The boundary holds every lazy module of a page: the layout and its
    // screen paint together, and a navigation keeps the old screen until the
    // new one loads, so no page paints half built (wamn-28n8).
    <Router root={(root) => <Suspense>{root.children}</Suspense>}>
      <Route path="/" component={() => <ChooseEnvironment title={props.title} scope={props} options={options} home={first?.path ?? ""} />} />
      <Route path="/invite" component={() => <AcceptInvitation title={props.title} options={options} />} />
      <Route path="/recover" component={() => <RecoverPassword title={props.title} options={options} />} />
      <Route path="/reset" component={() => <ResetPassword title={props.title} options={options} />} />
      <Route
        path="/:aud"
        component={(section: RouteSectionProps) => (
          <Session title={props.title} scope={props} sections={props.sections} options={options}>
            {section.children}
          </Session>
        )}
      >
        <Route path="/" component={() => (first === undefined ? <NotFound /> : <Navigate href={first.path} />)} />
        <For each={routes}>
          {({ route, home }) => <Route path={`/${route.path}`} component={screenRoute(route, home)} />}
        </For>
        <Route path="*" component={NotFound} />
      </Route>
      <Route path="*" component={NotFound} />
    </Router>
  );
}

/** The sign in form, which reports what the identity service refused. */
function SignInForm(props: {
  readonly submit: (email: string, password: string) => Promise<void>;
  readonly trouble: string | null;
}): JSX.Element {
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [refused, setRefused] = createSignal<string | null>(null);
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        setRefused(null);
        props.submit(email(), password()).catch((error: unknown) => setRefused(signInFailed(error)));
      }}
    >
      <FieldGroup>
        <Field>
          <FieldLabel for="shell-email">email</FieldLabel>
          <Input
            id="shell-email"
            type="email"
            autocomplete="username"
            value={email()}
            onInput={(event) => setEmail(event.currentTarget.value)}
          />
        </Field>
        <Field>
          <FieldLabel for="shell-password">password</FieldLabel>
          <Input
            id="shell-password"
            type="password"
            autocomplete="current-password"
            value={password()}
            onInput={(event) => setPassword(event.currentTarget.value)}
          />
        </Field>
        <Show when={refused() ?? props.trouble}>{(text) => <FieldError>{text()}</FieldError>}</Show>
        <Button type="submit">sign in</Button>
        <A href="/recover">forgot password</A>
      </FieldGroup>
    </form>
  );
}

function signInFailed(error: unknown): string {
  return `Sign in failed: ${error instanceof Error ? error.message : String(error)}.`;
}

/**
 * The page at `/invite#<code>`, which the invitation mail links to. The code,
 * `<principal id>:<secret>`, is in the fragment, so it never reaches a server
 * log. The page sets the first password and goes to the sign in page.
 */
function AcceptInvitation(props: { readonly title: string; readonly options: SessionOptions }): JSX.Element {
  const code = window.location.hash.slice(1);
  const split = code.indexOf(":");
  return (
    <NewPassword
      title={props.title}
      id="invite"
      missing={split > 0 ? null : "This address holds no invitation code. Open the link in the invitation mail."}
      failed="Sign up failed"
      submit={(_, password) => enroll(code.slice(0, split), code.slice(split + 1), password, props.options)}
    />
  );
}

/**
 * The page at `/reset#<secret>`, which the reset mail links to. The secret is
 * in the fragment, so it never reaches a server log. The page replaces the
 * password and goes to the sign in page.
 */
function ResetPassword(props: { readonly title: string; readonly options: SessionOptions }): JSX.Element {
  const secret = window.location.hash.slice(1);
  return (
    <NewPassword
      title={props.title}
      id="reset"
      missing={secret === "" ? "This address holds no reset code. Open the link in the reset mail." : null}
      failed="Reset failed"
      submit={(email, password) => resetPassword(email, secret, password, props.options)}
    />
  );
}

/**
 * The form of a new password: the email, and the password twice. With a code
 * in the address, it submits and goes to the sign in page, and it shows what
 * the identity service refused.
 */
function NewPassword(props: {
  readonly title: string;
  /** The prefix of the field ids. */
  readonly id: string;
  /** The text when the address holds no code, or null. */
  readonly missing: string | null;
  /** The start of the refusal text, such as `Sign up failed`. */
  readonly failed: string;
  readonly submit: (email: string, password: string) => Promise<void>;
}): JSX.Element {
  const navigate = useNavigate();
  const [email, setEmail] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [again, setAgain] = createSignal("");
  const [differ, setDiffer] = createSignal(false);
  const [refused, setRefused] = createSignal<string | null>(null);
  const trouble = () =>
    props.missing ??
    (differ() ? "The two passwords differ." : null) ??
    (refused() === null ? null : `${props.failed}: ${refused()}.`);
  return (
    <CardPage title={props.title}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (props.missing !== null) {
            return;
          }
          setDiffer(password() !== again());
          setRefused(null);
          if (differ()) {
            return;
          }
          props
            .submit(email(), password())
            .then(() => navigate("/", { replace: true }))
            .catch((error: unknown) => setRefused(error instanceof Error ? error.message : String(error)));
        }}
      >
        <FieldGroup>
          <Field>
            <FieldLabel for={`${props.id}-email`}>email</FieldLabel>
            <Input
              id={`${props.id}-email`}
              type="email"
              autocomplete="username"
              value={email()}
              onInput={(event) => setEmail(event.currentTarget.value)}
            />
          </Field>
          <Field>
            <FieldLabel for={`${props.id}-password`}>password</FieldLabel>
            <Input
              id={`${props.id}-password`}
              type="password"
              autocomplete="new-password"
              value={password()}
              onInput={(event) => setPassword(event.currentTarget.value)}
            />
          </Field>
          <Field>
            <FieldLabel for={`${props.id}-again`}>password again</FieldLabel>
            <Input
              id={`${props.id}-again`}
              type="password"
              autocomplete="new-password"
              value={again()}
              onInput={(event) => setAgain(event.currentTarget.value)}
            />
          </Field>
          <Show when={trouble()}>{(text) => <FieldError>{text()}</FieldError>}</Show>
          <Button type="submit">set password</Button>
        </FieldGroup>
      </form>
    </CardPage>
  );
}

/**
 * The page at `/recover`, which the sign in page links to. It asks identity
 * to mail a reset link to the address. Identity answers the same for every
 * address, so the page says the same for every address.
 */
function RecoverPassword(props: { readonly title: string; readonly options: SessionOptions }): JSX.Element {
  const [email, setEmail] = createSignal("");
  const [sent, setSent] = createSignal(false);
  const [refused, setRefused] = createSignal<string | null>(null);
  return (
    <CardPage title={props.title}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          setRefused(null);
          recover(email(), props.options)
            .then(() => setSent(true))
            .catch((error: unknown) =>
              setRefused(`Recovery failed: ${error instanceof Error ? error.message : String(error)}.`),
            );
        }}
      >
        <FieldGroup>
          <Field>
            <FieldLabel for="recover-email">email</FieldLabel>
            <Input
              id="recover-email"
              type="email"
              autocomplete="username"
              value={email()}
              onInput={(event) => setEmail(event.currentTarget.value)}
            />
          </Field>
          <Show when={refused()}>{(text) => <FieldError>{text()}</FieldError>}</Show>
          <Show when={sent()}>
            <p>If this address has an account, a mail with a reset link is on its way.</p>
          </Show>
          <Button type="submit">send reset link</Button>
          <A href="/">sign in</A>
        </FieldGroup>
      </form>
    </CardPage>
  );
}

/** The org and the project of the application. */
interface Scope {
  readonly org: string;
  readonly project: string;
}

/** Whether an audience, `urn:wamn:project-env:<org>:<project>:<env>:<instance>`, is one of the project. */
function ofProject(aud: string, scope: Scope): boolean {
  return aud.startsWith(`urn:wamn:project-env:${scope.org}:${scope.project}:`);
}

/**
 * The page at `/`: identity lists the environments of the application's
 * project that the account can reach. With one, the account signs in to it at
 * once. With more, the one it chooses signs it in. The environment becomes
 * the first segment of the address.
 */
function ChooseEnvironment(props: {
  readonly title: string;
  readonly scope: Scope;
  readonly options: SessionOptions;
  /** The path of the first screen, or "" when there is none. */
  readonly home: string;
}): JSX.Element {
  const navigate = useNavigate();
  const [credentials, setCredentials] = createSignal<{ email: string; password: string } | null>(null);
  const [reachable, setReachable] = createSignal<readonly Environment[]>([]);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const enter = (email: string, password: string, aud: string) => {
    setTrouble(null);
    // Straight to the first screen: the redirect of the environment's own
    // address would paint the layout with no screen for one step (wamn-28n8).
    signIn(email, password, aud, props.options)
      .then(() => navigate(props.home === "" ? `/${aud}` : `/${aud}/${props.home}`))
      .catch((error: unknown) => setTrouble(signInFailed(error)));
  };
  return (
    <CardPage title={props.title}>
      <SignInForm
        trouble={null}
        submit={async (email, password) => {
          const found = await environments(email, password, props.scope, props.options);
          setReachable(found);
          setCredentials({ email, password });
          const only = found.length === 1 ? found[0] : undefined;
          if (only !== undefined) {
            enter(email, password, only.aud);
          }
        }}
      />
      <Show when={credentials()}>
        {(held) => (
          <FieldGroup>
            <Show when={reachable().length === 0}>
              <FieldError>This account reaches no environment of this application.</FieldError>
            </Show>
            <For each={reachable().length > 1 ? reachable() : []}>
              {(environment) => (
                <Button variant="outline" onClick={() => enter(held().email, held().password, environment.aud)}>
                  {environment.org}/{environment.project}/{environment.env}
                </Button>
              )}
            </For>
            <Show when={trouble()}>{(text) => <FieldError>{text()}</FieldError>}</Show>
          </FieldGroup>
        )}
      </Show>
    </CardPage>
  );
}

/**
 * Everything below one environment. The keeper renews the session at once, so
 * a reload or a pasted address signs in again from the renewal cookie. With no
 * session, the page asks for the password on the same address, and the screen
 * shows once it is given.
 */
function Session(props: {
  readonly title: string;
  readonly scope: Scope;
  readonly sections: readonly ShellSection[];
  readonly options: SessionOptions;
  readonly children?: JSX.Element;
}): JSX.Element {
  const params = useParams<{ aud: string }>();
  return (
    <Show
      when={ofProject(params.aud ?? "", props.scope) ? params.aud : undefined}
      keyed
      fallback={
        <CardPage title={props.title}>
          <FieldError>This address names an environment of another application.</FieldError>
          <A href="/">sign in</A>
        </CardPage>
      }
    >
      {(aud) => {
        const [state, setState] = createSignal<SessionState | null>(null);
        const keeper = keepSession({ ...props.options, aud, onState: setState });
        onCleanup(() => keeper.stop());
        const transport = createTransport({ ...props.options, baseUrl: API_BASE, cookie: true });
        const current = () => state();
        // The layout module loads while the session renews (wamn-28n8).
        void Layout.preload();
        return (
          <Switch>
            <Match when={current()?.status === "signedIn"}>
              <TransportContext.Provider value={transport}>
                <Layout title={props.title} sections={props.sections} aud={aud} signOut={() => keeper.signOut()}>
                  {props.children}
                </Layout>
              </TransportContext.Provider>
            </Match>
            <Match when={current() !== null}>
              <CardPage title={props.title}>
                <SignInForm trouble={failure(current())} submit={(email, password) => keeper.signIn(email, password)} />
              </CardPage>
            </Match>
          </Switch>
        );
      }}
    </Show>
  );
}

function failure(state: SessionState | null): string | null {
  return state?.status === "failed" ? `The session could not be renewed: ${state.reason}.` : null;
}

function NotFound(): JSX.Element {
  return (
    <CardPage title="No page here">
      <p>This address names no page.</p>
      <A href="/">Go to sign in</A>
    </CardPage>
  );
}
