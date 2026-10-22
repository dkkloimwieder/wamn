import { createSignal, lazy, onCleanup, Show, Loading, type Component } from "solid-js";
import type { JSX } from "@solidjs/web";
import { DashboardPanel, type DashboardShellProps } from "@wfe/components/layout";
import { permission } from "@wamn/control-client";
import { control, environment, member, orgAdmin, project, projectAdmin, release, user } from "@wamn/control-org-client";
import {
  createTransport,
  keepSession,
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

export interface ScreenProps {
  readonly transport: Transport;
  readonly holds: Holds;
  readonly controlMine?: control.ControlMineResult;
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
  readonly controlMine?: control.ControlMineResult;
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
          <NotFoundBoundary home={`/${props.aud}`} path={props.address.url().pathname}>
            <Layout
              title={props.kind === "control" ? props.org : props.title}
              aud={props.aud}
              sections={shown().sections}
              holds={shown().holds}
              controlMine={shown().controlMine}
              transport={transport}
              address={props.address}
              signOut={() => keeper.signOut()}
              routes={sections}
            />
          </NotFoundBoundary>
        )}
      </Show>
    </Show>
  );
}

function failure(state: SessionState | null): string | null {
  return state?.status === "failed" ? `The session could not be renewed: ${state.reason}.` : null;
}
