import { createMemo } from "solid-js";
import { StatusTile, TileRow } from "@wfe/components";
import { controlFields, type EnvStatusResult } from "./control-client";

export function EnvironmentTiles(props: { status?: EnvStatusResult; error?: string }) {
  const complete = () => {
    const rollout = props.status?.rollout;
    return (
      rollout &&
      [rollout.updated_replicas, rollout.ready_replicas, rollout.available_replicas].every(
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
  return (
    <TileRow role="region" aria-label="Environment status">
      <StatusTile
        label={controlFields.status.rollout.title}
        value={
          props.status
            ? `${props.status.rollout.ready_replicas} ready / ${props.status.rollout.replicas} total`
            : "Unknown"
        }
        state={!props.status ? "unknown" : complete() ? "ok" : "warning"}
        detail={props.error}
      />
      <StatusTile
        label={controlFields.status.users_with_role.title}
        value={props.status?.users_with_role ?? "Unknown"}
        state={props.status?.users_with_role == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label={controlFields.status.roles.title}
        value={props.status?.roles ?? "Unknown"}
        state={props.status?.roles == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label={controlFields.status.memberships.title}
        value={props.status?.memberships ?? "Unknown"}
        state={props.status?.memberships == null ? "unknown" : "ok"}
        detail={props.error}
      />
      <StatusTile
        label={controlFields.status.workloads.title}
        value={workloads() ? `${workloads()!.ready} ready / ${workloads()!.notReady} not ready` : "Unknown"}
        state={!workloads() ? "unknown" : workloads()!.notReady ? "warning" : "ok"}
        detail={props.error}
      />
    </TileRow>
  );
}
