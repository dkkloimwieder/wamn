import { createSignal, For, lazy, onCleanup, Show, Loading, type Component } from "solid-js";
import type { JSX } from "@solidjs/web";
import { uiScope } from "@wfe/components";
import { Button } from "@wfe/components/fields";
import { permission } from "@wamn/control-client";
import { control, environment, member, project, projectAdmin, release, user } from "@wamn/control-org-client";
import {
  createTransport,
  keepSession,
  type SessionOptions,
  type SessionState,
  type Transport,
} from "@wamn/web-runtime";
import { audienceKind, createAddress, matchPath, queryValues, type Address, type Kind } from "./address";
import type { Scope } from "./scope";
import { identityPage, SignInForm } from "./identity";
import { Page, NotFound } from "./page";

const Layout = lazy(() => import("./layout"));

export const API_BASE = "/api";

export interface ScreenProps {
  readonly transport: Transport;
  readonly holds: Holds;
  readonly controlProjects?: readonly string[];
  readonly params: Readonly<Record<string, string | undefined>>;
  readonly search: Readonly<Record<string, string | undefined>>;
  readonly open: (path: string) => void;
  readonly close: () => void;
}
export interface ShellAction {
  readonly label: string;
  readonly path: string;
  readonly operation: string;
}
export interface ShellRoute {
  readonly path: string;
  readonly operation: string;
  readonly component: Component<ScreenProps>;
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
  readonly controlProjects?: readonly string[];
}

function reference(operation: string): string {
  const at = operation.lastIndexOf("@");
  return at < 0 ? operation : operation.slice(0, at);
}

async function applicationView(transport: Transport, sections: readonly ShellSection[]): Promise<View> {
  const outcome = await permission.mine(transport, [{}]);
  const held = new Set(outcome.status === "completed" ? outcome.value.permissions : []);
  const admin = outcome.status === "completed" && outcome.value.admin;
  const holds: Holds = (operation) => admin || held.has(reference(operation));
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

export function controlHolds(mine: control.ControlMineResult): Holds {
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
    controlProjects: projects,
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
    <div {...uiScope()}>
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
                  fallback={
                    <Page title={props.title}>
                      <p>This address names an environment of another org.</p>
                      <a href="/">sign in</a>
                    </Page>
                  }
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
    </div>
  );
}

function Session(
  props: ShellProps & { aud: string; kind: Kind; options: SessionOptions; address: Address },
): JSX.Element {
  const [state, setState] = createSignal<SessionState | null>(null);
  const [view, setView] = createSignal<View>();
  const [trouble, setTrouble] = createSignal<string>();
  const sections = props.kind === "control" ? (props.controlSections ?? []) : props.sections;
  const transport = createTransport({
    ...props.options,
    baseUrl: API_BASE,
    cookie: true,
  });
  let active = true;
  let generation = 0;
  let signedIn = false;
  const keeper = keepSession({
    ...props.options,
    aud: props.aud,
    onState: (next) => {
      setState(next);
      if (next.status !== "signedIn") {
        signedIn = false;
        generation += 1;
        setView(undefined);
      } else if (!signedIn) {
        signedIn = true;
        const current = ++generation;
        setTrouble(undefined);
        (props.kind === "control" ? controlView(transport, sections) : applicationView(transport, sections)).then(
          (shown) => {
            if (!active || generation !== current) return;
            setView(shown);
            const first = shown.sections[0]?.screens[0];
            if (props.address.url().pathname.replace(/\/$/, "") === `/${props.aud}` && first) {
              props.address.navigate(`/${props.aud}/${first.path}`, true);
            }
          },
          (error: unknown) => {
            if (active && generation === current) setTrouble(String(error));
          },
        );
      }
    },
  });
  void Layout.preload();
  onCleanup(() => {
    active = false;
    keeper.stop();
  });
  return (
    <Show
      when={state()?.status === "signedIn"}
      fallback={
        <Page title={props.title}>
          <Show when={state() !== null}>
            <SignInForm
              address={props.address}
              trouble={failure(state())}
              submit={(email, password) => keeper.signIn(email, password)}
            />
          </Show>
        </Page>
      }
    >
      <Show when={trouble()}>{(text) => <p role="alert">{text()}</p>}</Show>
      <Show when={view()}>
        {(shown) => (
          <Layout
            title={props.kind === "control" ? props.org : props.title}
            aud={props.aud}
            sections={shown().sections}
            address={props.address}
            signOut={() => keeper.signOut()}
          >
            <Destination {...props} sections={sections} view={shown()} transport={transport} />
          </Layout>
        )}
      </Show>
    </Show>
  );
}

function Destination(props: {
  aud: string;
  kind: Kind;
  address: Address;
  sections: readonly ShellSection[];
  view: View;
  transport: Transport;
}): JSX.Element {
  const path = () => props.address.url().pathname.split("/").slice(2).join("/");
  const selected = () => {
    for (const section of props.sections)
      for (const route of [...section.screens, ...(section.routes ?? [])]) {
        const params = matchPath(route.path, path());
        if (!params || !props.view.holds(route.operation, params.project)) continue;
        params.aud = props.aud;
        return { route, home: section.screens[0], params };
      }
    return undefined;
  };
  return (
    <Show
      when={selected()}
      keyed
      fallback={
        <Show when={path() !== ""}>
          <NotFound />
        </Show>
      }
    >
      {({ route, home, params }) => {
        const open = (path: string) => props.address.navigate(`/${props.aud}/${path}`, false, true);
        const close = () =>
          window.history.state?.opened === true
            ? window.history.back()
            : props.address.navigate(
                `/${props.aud}/${home && props.view.holds(home.operation) ? home.path : (props.view.sections[0]?.screens[0]?.path ?? "")}`,
                true,
              );
        return (
          <>
            <For each={(route.actions ?? []).filter((action) => props.view.holds(action.operation))}>
              {(action) => (
                <Button
                  onClick={() =>
                    open(action.path.replace(/:(\w+)/g, (_, name: string) => encodeURIComponent(params[name] ?? "")))
                  }
                >
                  {action.label}
                </Button>
              )}
            </For>
            <route.component
              transport={props.transport}
              holds={(operation, project) => props.view.holds(operation, project ?? params.project)}
              controlProjects={props.view.controlProjects}
              params={params}
              search={queryValues(props.address.url().searchParams)}
              open={open}
              close={close}
            />
          </>
        );
      }}
    </Show>
  );
}

function failure(state: SessionState | null): string | null {
  return state?.status === "failed" ? `The session could not be renewed: ${state.reason}.` : null;
}
