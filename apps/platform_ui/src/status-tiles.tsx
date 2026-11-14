import { createMemo } from "solid-js";
import { StatusTile, TileRow } from "@wfe/components";
import type { environment } from "./generated/wamn_control_org/index.js";

export function EnvironmentTiles(props: { status?: environment.EnvironmentStatusResult; error?: string }) {
  const complete = () => {
    const rollout = props.status?.rollout;
    return (
      rollout &&
      [rollout.updatedReplicas, rollout.readyReplicas, rollout.availableReplicas].every(
        (count) => count === rollout.replicas,
      )
    );
  };
  const workloads = createMemo(() => {
    const entries = props.status?.workloads;
    if (!entries) return undefined;
    const ready = entries.filter((entry) =>
      entry.conditions.some((condition) => condition.type === "Ready" && condition.status === "True"),
    ).length;
    return { ready, notReady: entries.length - ready };
  });
  const residency = createMemo(() => {
    const entries = props.status?.residency;
    if (!entries) return undefined;
    const loaded = entries.filter((entry) => entry.loaded).length;
    return { loaded, notLoaded: entries.length - loaded };
  });
  return (
    <TileRow role="region" aria-label="Environment status">
      <StatusTile
        label="Resident releases"
        value={residency() ? `${residency()!.loaded} loaded / ${residency()!.notLoaded} not loaded` : "Unknown"}
        state={!residency() ? "unknown" : residency()!.notLoaded || !complete() ? "warning" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label="Users with a role"
        value={props.status?.usersWithRole ?? "Unknown"}
        state={props.status?.usersWithRole == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label="Roles"
        value={props.status?.roles ?? "Unknown"}
        state={props.status?.roles == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label="Memberships"
        value={props.status?.memberships ?? "Unknown"}
        state={props.status?.memberships == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label="Workloads"
        value={workloads() ? `${workloads()!.ready} ready / ${workloads()!.notReady} not ready` : "Unknown"}
        state={!workloads() ? "unknown" : workloads()!.notReady ? "warning" : "ok"}
        detail={props.error}
      />
    </TileRow>
  );
}
