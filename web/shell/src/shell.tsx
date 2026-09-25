/**
 * The app shell: sign in, the session, the layout and the routes.
 *
 * The first segment of every address is the environment, as its audience, so
 * a reload renews the session with nothing in browser storage. Each screen
 * route sits below that segment and renders inside the layout. The shell owns
 * no screen: the application writes each one from its generated components.
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
  Match,
  onCleanup,
  Show,
  Switch,
  useContext,
  type Component,
  type JSX,
} from "solid-js";

import {
  AppFrame,
  Button,
  CardPage,
  Field,
  FieldError,
  FieldGroup,
  FieldLabel,
  Input,
  useColorMode,
  type FrameEntry,
  type FrameItem,
} from "@wamn/ui";
import {
  createTransport,
  environments,
  keepSession,
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
}

/** One screen of the application, on its own route. */
export interface ShellScreen {
  /** The path below the environment, with no leading slash, such as `pallets`. */
  readonly path: string;
  /** The navigation label, from the screen label the generated module exports. */
  readonly label: string;
  readonly component: Component<ScreenProps>;
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
}

export interface ShellProps {
  /** The application name, in the sidebar and on the sign in card. */
  readonly title: string;
  readonly sections: readonly ShellSection[];
  /** The fetch every call uses. The global one is the default. */
  readonly fetch?: typeof globalThis.fetch;
}

const TransportContext = createContext<Transport>();

/**
 * One screen on its route, with the transport of the session it renders in.
 * The transport is read here, while the route renders. A component reads its
 * prop later, in an event handler, where no context is in reach.
 */
function screenRoute(screen: ShellScreen): Component {
  return () => {
    const transport = useContext(TransportContext);
    if (transport === undefined) {
      throw new Error("a screen renders outside a signed-in session");
    }
    return <screen.component transport={transport} />;
  };
}

export function Shell(props: ShellProps): JSX.Element {
  const options: SessionOptions = props.fetch === undefined ? {} : { fetch: props.fetch };
  const screens = props.sections.flatMap((section) => section.screens);
  const first = screens[0];
  return (
    <Router>
      <Route path="/" component={() => <ChooseEnvironment title={props.title} options={options} />} />
      <Route
        path="/:aud"
        component={(section: RouteSectionProps) => (
          <Session title={props.title} sections={props.sections} options={options}>
            {section.children}
          </Session>
        )}
      >
        <Route path="/" component={() => (first === undefined ? <NotFound /> : <Navigate href={first.path} />)} />
        {screens.map((screen) => (
          <Route path={`/${screen.path}`} component={screenRoute(screen)} />
        ))}
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
      </FieldGroup>
    </form>
  );
}

function signInFailed(error: unknown): string {
  return `Sign in failed: ${error instanceof Error ? error.message : String(error)}.`;
}

/**
 * The page at `/`: an account lists the environments it can reach, and the
 * one it chooses signs it in and becomes the first segment of the address.
 */
function ChooseEnvironment(props: { readonly title: string; readonly options: SessionOptions }): JSX.Element {
  const navigate = useNavigate();
  const [credentials, setCredentials] = createSignal<{ email: string; password: string } | null>(null);
  const [reachable, setReachable] = createSignal<readonly Environment[]>([]);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  return (
    <CardPage title={props.title}>
      <SignInForm
        trouble={null}
        submit={async (email, password) => {
          setReachable(await environments(email, password, props.options));
          setCredentials({ email, password });
        }}
      />
      <Show when={credentials()}>
        {(held) => (
          <FieldGroup>
            <Show when={reachable().length === 0}>
              <FieldError>This account reaches no environment.</FieldError>
            </Show>
            <For each={reachable()}>
              {(environment) => (
                <Button
                  variant="outline"
                  onClick={() => {
                    setTrouble(null);
                    signIn(held().email, held().password, environment.aud, props.options)
                      .then(() => navigate(`/${environment.aud}`))
                      .catch((error: unknown) => setTrouble(signInFailed(error)));
                  }}
                >
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
  readonly sections: readonly ShellSection[];
  readonly options: SessionOptions;
  readonly children?: JSX.Element;
}): JSX.Element {
  const params = useParams<{ aud: string }>();
  return (
    <Show when={params.aud} keyed>
      {(aud) => {
        const [state, setState] = createSignal<SessionState | null>(null);
        const keeper = keepSession({ ...props.options, aud, onState: setState });
        onCleanup(() => keeper.stop());
        const transport = createTransport({ ...props.options, baseUrl: API_BASE, cookie: true });
        const current = () => state();
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
                <SignInForm
                  trouble={failure(current())}
                  submit={(email, password) => keeper.signIn(email, password)}
                />
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

function Layout(props: {
  readonly title: string;
  readonly sections: readonly ShellSection[];
  readonly aud: string;
  readonly signOut: () => Promise<void>;
  readonly children?: JSX.Element;
}): JSX.Element {
  const location = useLocation();
  const { colorMode, toggleColorMode } = useColorMode();
  const item = (label: string, screen: ShellScreen): FrameItem => ({
    label,
    href: screen.path,
    active: () => {
      const own = `/${props.aud}/${screen.path}`;
      return location.pathname === own || location.pathname.startsWith(`${own}/`);
    },
  });
  const navigation: FrameEntry[] = props.sections.map((section) => {
    const only = section.screens.length === 1 ? section.screens[0] : undefined;
    return only === undefined
      ? { label: section.label, items: section.screens.map((screen) => item(screen.label, screen)) }
      : item(section.label, only);
  });
  return (
    <AppFrame
      title={props.title}
      navigation={navigation}
      link={A}
      context={props.aud}
      actions={
        <>
          <Button variant="outline" size="sm" onClick={toggleColorMode}>
            {colorMode() === "dark" ? "light mode" : "dark mode"}
          </Button>
          <Button variant="outline" size="sm" onClick={() => void props.signOut()}>
            sign out
          </Button>
        </>
      }
    >
      {props.children}
    </AppFrame>
  );
}

function NotFound(): JSX.Element {
  return (
    <CardPage title="No page here">
      <p>This address names no page.</p>
      <A href="/">Go to sign in</A>
    </CardPage>
  );
}
