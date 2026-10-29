import { createContext, createSignal, lazy, onCleanup, Show, Loading, useContext, type Component } from "solid-js";
import type { JSX } from "@solidjs/web";
import { DashboardPanel, type DashboardShellProps } from "@wfe/components/layout";
import { permission } from "./generated/wamn_control/index.js";
import {
  control,
  environment,
  member,
  orgAdmin,
  project,
  projectAdmin,
  release,
  user,
} from "./generated/wamn_control_org/index.js";
import {
  createTransport,
  deleteDatabase,
  keepSession,
  offlineSettings,
  opensOffline,
  outboxDatabase,
  rowsDatabase,
  savedUser,
  savedView,
  type OfflineSettings,
  type SavedView,
  type SessionOptions,
  type SessionState,
  type Transport,
} from "@wamn/web-runtime";
import { audienceKind, createAddress, type Address, type Kind } from "./address";
import type { Scope } from "./scope";
import { identityPage, SignInForm } from "./identity";
import { EntryPage, Frame, NotFound, NotFoundBoundary } from "./page";

const Layout = lazy(() => import("./layout"));

export const API_BASE = "/api";

/**
 * The user of the session the shell holds. The shell provides it while signed
 * in, and offline for the last signed-in user before renewal succeeds.
 */
export interface Session {
  /** The session reply's `sub`. */
  readonly userId: string;
  /** True while the shell shows the saved user, before renewal succeeds. */
  readonly offline: () => boolean;
  /** The offline profile settings saved with the user, or the `session` defaults. */
  readonly settings: OfflineSettings;
  /** Run before sign-out clears the user's saved rows and outbox, for example to close the outbox. */
  readonly onSignOut: (run: () => void) => void;
}

const SessionContext = createContext<Session | null>(null);

/** The signed-in user, or undefined outside a signed-in shell. */
export function useSession(): Session | undefined {
  return useContext(SessionContext) ?? undefined;
}

export interface ScreenProps {
  readonly transport: Transport;
  readonly holds: Holds;
  readonly controlMine?: Pick<control.ControlMineResult, "orgAdmin" | "projects">;
  readonly params: Readonly<Record<string, string | undefined>>;
  readonly search: Readonly<Record<string, string | undefined>>;
  readonly open: (path: string) => void;
  readonly close: () => void;
  readonly actions?: readonly ShellAction[];
}
export interface ShellAction {
  readonly label: string;
  readonly path: string;
  readonly operation: string;
}
export interface ShellRoute {
  readonly label?: string;
  readonly path: string;
  readonly operation: string;
  readonly component: Component<ScreenProps>;
  readonly scroll?: DashboardShellProps["scroll"];
  readonly actions?: readonly ShellAction[];
}
export interface ShellScreen extends ShellRoute {
  readonly label: string;
}
export interface ShellSection {
  readonly label: string;
  readonly screens: readonly ShellScreen[];
  readonly routes?: readonly ShellRoute[];
}
export interface ShellProps extends Scope {
  readonly title: string;
  readonly sections: readonly ShellSection[];
  readonly controlSections?: readonly ShellSection[];
  readonly fetch?: typeof globalThis.fetch;
}
type Holds = (operation: string, project?: string) => boolean;
interface View {
  readonly holds: Holds;
  readonly sections: readonly ShellSection[];
  readonly controlMine?: Pick<control.ControlMineResult, "orgAdmin" | "projects">;
}

function reference(operation: string): string {
  const at = operation.lastIndexOf("@");
  return at < 0 ? operation : operation.slice(0, at);
}

/** What the user holds, or undefined when the read did not reach the host. */
async function applicationHeld(transport: Transport, userId: string): Promise<SavedView | undefined> {
  const outcome = await permission.mine(transport, [{}]);
  if (outcome.status === "uncertain") return undefined;
  return outcome.status === "completed"
    ? { userId, held: outcome.value.permissions, admin: outcome.value.admin }
    : { userId, held: [], admin: false };
}

function applicationView(saved: SavedView, sections: readonly ShellSection[]): View {
  const held = new Set(saved.held);
  const holds: Holds = (operation) => saved.admin || held.has(reference(operation));
  return {
    holds,
    sections: sections
      .map((section) => ({
        ...section,
        screens: section.screens.filter((screen) => holds(screen.operation)),
        routes: (section.routes ?? []).filter((route) => holds(route.operation)),
      }))
      .filter((section) => section.screens.length > 0),
  };
}

export function controlHolds(mine: Pick<control.ControlMineResult, "orgAdmin" | "projects">): Holds {
  const projectOperations = new Set([
    environment.ENVIRONMENT_LIST_ROUTE.operation,
    environment.ENVIRONMENT_STATUS_ROUTE.operation,
    release.RELEASE_PROFILE_ROUTE.operation,
    member.MEMBER_LIST_ROUTE.operation,
    member.MEMBER_GRANT_ROUTE.operation,
    member.MEMBER_REVOKE_ROUTE.operation,
    projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
    projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation,
  ]);
  const orgOperations = new Set([
    user.USER_LIST_ROUTE.operation,
    user.USER_INVITE_ROUTE.operation,
    user.USER_ACTIVATE_ROUTE.operation,
    user.USER_DEACTIVATE_ROUTE.operation,
    orgAdmin.ORG_ADMIN_GRANT_ROUTE.operation,
    orgAdmin.ORG_ADMIN_REVOKE_ROUTE.operation,
    project.PROJECT_LIST_ROUTE.operation,
    project.PROJECT_CREATE_ROUTE.operation,
    project.PROJECT_ACTIVATE_ROUTE.operation,
    project.PROJECT_INACTIVATE_ROUTE.operation,
    environment.ENVIRONMENT_ACTIVATE_ROUTE.operation,
    environment.ENVIRONMENT_INACTIVATE_ROUTE.operation,
  ]);
  return (operation, scope) => {
    if (operation === control.CONTROL_MINE_ROUTE.operation)
      return mine.orgAdmin || mine.projects.some((entry) => entry.projectAdmin);
    if (orgOperations.has(operation)) return mine.orgAdmin;
    return (
      projectOperations.has(operation) &&
      (mine.orgAdmin ||
        mine.projects.some((entry) => entry.projectAdmin && (scope === undefined || entry.project === scope)))
    );
  };
}

async function controlView(transport: Transport, sections: readonly ShellSection[]): Promise<View> {
  const outcome = await control.mine(transport, [{}]);
  if (outcome.status !== "completed") return { holds: () => false, sections: [] };
  const projects = outcome.value.projects.filter((entry) => entry.projectAdmin).map((entry) => entry.project);
  const holds = controlHolds(outcome.value);
  return {
    holds,
    controlMine: outcome.value,
    sections: sections
      .map((section) => ({
        ...section,
        screens: section.screens
          .filter((screen) => holds(screen.operation))
          .flatMap((screen) =>
            screen.path.includes(":project")
              ? projects.map((project) => ({
                  ...screen,
                  path: screen.path.replace(":project", encodeURIComponent(project)),
                  label: project,
                }))
              : [screen],
          ),
      }))
      .filter((section) => section.screens.length > 0),
  };
}

export function screen<M>(load: () => Promise<M>, pick: (module: M) => Component<ScreenProps>): Component<ScreenProps> {
  return lazy(async () => ({ default: pick(await load()) }));
}

export function Shell(props: ShellProps): JSX.Element {
  const address = createAddress();
  const options: SessionOptions = props.fetch === undefined ? {} : { fetch: props.fetch };
  const audience = () => address.url().pathname.split("/")[1] ?? "";
  return (
    <Loading>
      <Show
        when={identityPage(address.url().pathname)}
        keyed
        fallback={
          <Show when={audience()} keyed fallback={<NotFound />}>
            {(aud) => (
              <Show
                when={audienceKind(aud, props)}
                keyed
                fallback={<NotFound message="This address names an environment of another org." />}
              >
                {(kind) => <Session {...props} aud={aud} kind={kind} options={options} address={address} />}
              </Show>
            )}
          </Show>
        }
      >
        {(Identity) => <Identity title={props.title} scope={props} options={options} address={address} />}
      </Show>
    </Loading>
  );
}

/** The saved user and view of an application environment. The control audience saves none. */
function saved(aud: string) {
  const match = /^urn:wamn:project-env:[^:]+:([^:]+):([^:]+):[^:]+$/.exec(aud);
  if (!match) return undefined;
  const application = match[1]!;
  const environment = match[2]!;
  return {
    user: savedUser(application, environment),
    view: savedView(application, environment),
    /** Delete the user's saved rows and outbox. */
    clear: (userId: string) =>
      Promise.all([
        deleteDatabase(outboxDatabase(application, environment, userId)),
        deleteDatabase(rowsDatabase(application, environment, userId)),
      ]),
  };
}

function Session(
  props: ShellProps & { aud: string; kind: Kind; options: SessionOptions; address: Address },
): JSX.Element {
  const [state, setState] = createSignal<SessionState | null>(null);
  const [view, setView] = createSignal<View>();
  const [trouble, setTrouble] = createSignal<string>();
  // The last signed-in user, shown while the first renewal cannot reach the identity service.
  const [offlineUser, setOfflineUser] = createSignal<string>();
  const store = props.kind === "application" ? saved(props.aud) : undefined;
  const userId = () => {
    const current = state();
    return current?.status === "signedIn" ? current.userId : offlineUser();
  };
  const sections = props.kind === "control" ? (props.controlSections ?? []) : props.sections;
  const transport = createTransport({
    ...props.options,
    baseUrl: API_BASE,
    cookie: true,
  });
  let active = true;
  let generation = 0;
  let signedIn = false;
  // Under the session profile an offline reopen ends when the saved login expires.
  let loginEnds: ReturnType<typeof setTimeout> | undefined;
  const signOutRuns = new Set<() => void>();
  const settings = () => offlineSettings(store?.user.load());
  function show(shown: View): void {
    setView(shown);
    const first = shown.sections[0]?.screens[0];
    if (props.address.url().pathname.replace(/\/$/, "") === `/${props.aud}` && first) {
      const projects = shown.controlMine?.projects.filter((entry) =>
        shown.holds(member.MEMBER_LIST_ROUTE.operation, entry.project),
      );
      const home =
        projects?.length === 1
          ? `projects/${encodeURIComponent(projects[0]!.project)}`
          : shown.sections.some((section) => section.routes?.some((route) => route.path === ""))
            ? undefined
            : first.path;
      if (home !== undefined) props.address.navigate(`/${props.aud}/${home}`, true);
    }
  }
  /** The saved view of the user, for a shell that cannot reach the host. */
  function savedViewOf(id: string): View | undefined {
    const held = store?.view.load(id);
    return held === undefined ? undefined : applicationView(held, sections);
  }
  async function readView(id: string): Promise<View> {
    if (props.kind === "control") return controlView(transport, sections);
    const held = await applicationHeld(transport, id);
    if (held !== undefined) {
      store?.view.save(held);
      return applicationView(held, sections);
    }
    const fallback = view() ?? savedViewOf(id);
    if (fallback === undefined) throw new Error("The permissions could not be read. Try again when online.");
    return fallback;
  }
  const keeper = keepSession({
    ...props.options,
    aud: props.aud,
    onState: (next) => {
      setState(next);
      clearTimeout(loginEnds);
      if (next.status === "unreachable") {
        const user = store?.user.load();
        const shown = user === undefined ? undefined : savedViewOf(user.userId);
        if (user !== undefined && shown !== undefined && opensOffline(user, Date.now())) {
          setOfflineUser(user.userId);
          show(shown);
          if (offlineSettings(user).profile === "session") {
            // After the saved login expires, the shell shows the sign-in form.
            loginEnds = setTimeout(
              () => {
                if (state()?.status !== "signedIn") setOfflineUser(undefined);
              },
              Math.min(user.loginExpiresAt * 1000 - Date.now(), 2_147_483_647),
            );
          }
        }
      } else if (next.status !== "signedIn") {
        signedIn = false;
        generation += 1;
        setOfflineUser(undefined);
        setView(undefined);
      } else {
        // The settings saved with the same user stay until a settings read replaces them (wamn-yaiqo).
        const before = store?.user.load();
        store?.user.save({
          userId: next.userId,
          expiresAt: next.expiresAt,
          loginExpiresAt: next.loginExpiresAt,
          ...(before?.userId === next.userId && before.settings !== undefined ? { settings: before.settings } : {}),
        });
        if (signedIn) return;
        signedIn = true;
        setOfflineUser(undefined);
        const current = ++generation;
        setTrouble(undefined);
        readView(next.userId).then(
          (shown) => {
            if (active && generation === current) show(shown);
          },
          (error: unknown) => {
            if (active && generation === current) setTrouble(String(error));
          },
        );
      }
    },
  });
  /**
   * Sign out. Under the session profile the saved user goes too. With
   * `clear_on_sign_out`, the user's saved rows and outbox go as well.
   */
  async function signOut(): Promise<void> {
    const id = userId();
    const current = settings();
    for (const run of signOutRuns) run();
    signOutRuns.clear();
    try {
      await keeper.signOut();
    } finally {
      if (current.profile === "session") store?.user.clear();
      if (id !== undefined && current.clearOnSignOut) await store?.clear(id);
    }
  }
  void Layout.preload();
  onCleanup(() => {
    active = false;
    clearTimeout(loginEnds);
    keeper.stop();
  });
  return (
    <Show
      when={state()?.status === "signedIn" || offlineUser() !== undefined}
      fallback={
        <EntryPage title="Sign in">
          <Show when={state() !== null}>
            <SignInForm
              address={props.address}
              trouble={failure(state())}
              submit={(email, password) => keeper.signIn(email, password)}
            />
          </Show>
        </EntryPage>
      }
    >
      <Show
        when={view()}
        fallback={
          <Frame header={<span>{props.title}</span>}>
            <DashboardPanel>
              <Show when={trouble()}>{(text) => <p role="alert">{text()}</p>}</Show>
            </DashboardPanel>
          </Frame>
        }
      >
        {(shown) => (
          // A different user mounts the screens again, so no screen keeps the old user's state.
          <Show when={userId()} keyed>
            {(id) => (
              <SessionContext
                value={{
                  userId: id,
                  offline: () => state()?.status !== "signedIn",
                  settings: settings(),
                  onSignOut: (run) => void signOutRuns.add(run),
                }}
              >
                <NotFoundBoundary home={`/${props.aud}`} path={props.address.url().pathname}>
                  <Layout
                    title={props.kind === "control" ? props.org : props.title}
                    aud={props.aud}
                    sections={shown().sections}
                    holds={shown().holds}
                    controlMine={shown().controlMine}
                    transport={transport}
                    address={props.address}
                    signOut={signOut}
                    routes={sections}
                  />
                </NotFoundBoundary>
              </SessionContext>
            )}
          </Show>
        )}
      </Show>
    </Show>
  );
}

function failure(state: SessionState | null): string | null {
  return state?.status === "failed" || state?.status === "unreachable"
    ? `The session could not be renewed: ${state.reason}.`
    : null;
}
