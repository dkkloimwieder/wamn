#!/usr/bin/env python3
"""Reduce completed fresh-auth load and memory sidecars to JSON on stdout.

Pass distinct run roots with --before and --after. Each root must contain
journey/throughput/{service,human}-{1,2,3} from the existing runner.
The run labels describe the code phases. Each sample edge is before or after
one throughput step. Statistics compare the three repetitions at that edge.
memory.current is a sampled host cgroup value, not peak memory or RSS.
"""

import argparse
import json
import math
from pathlib import Path
import re
from statistics import median


CREDENTIALS = ("service", "human")
REPETITIONS = (1, 2, 3)
LAYERS = ("route", "nodb", "pg")
CONCURRENCY = (1, 4, 8, 16, 32, 64)
EDGES = ("before", "after")
UNITS = {
    "load1": "tasks, 1-minute machine load average",
    "load5": "tasks, 5-minute machine load average",
    "load15": "tasks, 15-minute machine load average",
    "MemAvailable": "KiB, machine /proc/meminfo (Linux kB means 1024 bytes)",
    "memory.current": "bytes, sampled host cgroup memory (not peak or RSS)",
}


def read_text(path):
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError(f"{path}: cannot read the required evidence file") from error


def read_json(path):
    try:
        value = json.loads(read_text(path))
    except json.JSONDecodeError as error:
        raise ValueError(f"{path}: invalid JSON") from error
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return value


def read_metrics(load_path, machine_path, host_path):
    fields = read_text(load_path).split()
    if (len(fields) != 5 or not re.fullmatch(r"[0-9]+/[0-9]+", fields[3])
            or not re.fullmatch(r"[0-9]+", fields[4])):
        raise ValueError(f"{load_path}: incomplete or invalid /proc/loadavg sample")
    try:
        loads = [float(field) for field in fields[:3]]
    except ValueError as error:
        raise ValueError(f"{load_path}: invalid load average") from error
    if any(not math.isfinite(value) or value < 0 for value in loads):
        raise ValueError(f"{load_path}: load averages must be finite and nonnegative")
    available = [line for line in read_text(machine_path).splitlines()
                 if line.startswith("MemAvailable:")]
    if len(available) != 1 or not re.fullmatch(r"MemAvailable:\s+[0-9]+\s+kB", available[0]):
        raise ValueError(f"{machine_path}: expected one MemAvailable value in kB")
    current = read_text(host_path).strip()
    if not re.fullmatch(r"[0-9]+", current):
        raise ValueError(f"{host_path}: expected one memory.current value in bytes")
    return dict(zip(("load1", "load5", "load15"), loads)) | {
        "MemAvailable": int(available[0].split()[1]),
        "memory.current": int(current),
    }


def read_run(root, run):
    samples = []
    sources = set()
    expected = {(layer, c) for layer in LAYERS for c in CONCURRENCY}
    for credential in CREDENTIALS:
        for repetition in REPETITIONS:
            directory = root / "journey" / "throughput" / f"{credential}-{repetition}"
            index_path = directory / "index.json"
            index = read_json(index_path)
            if (index.get("schema") != "wamn-throughput/v0.1"
                    or index.get("concurrency") != list(CONCURRENCY)
                    or not isinstance(index.get("layers"), list)
                    or [item.get("layer") if isinstance(item, dict) else None
                        for item in index["layers"]] != list(LAYERS)
                    or not isinstance(index.get("source"), str)
                    or not re.fullmatch(r"[0-9a-f]{40}", index["source"])):
                raise ValueError(f"{index_path}: unsupported or incomplete sweep index")
            sources.add(index["source"])
            steps = index.get("steps")
            if not isinstance(steps, list):
                raise ValueError(f"{index_path}: missing completed steps")
            seen = set()
            for step in steps:
                if (not isinstance(step, dict) or step.get("layer") not in LAYERS
                        or type(step.get("concurrency")) is not int):
                    raise ValueError(f"{index_path}: invalid step coordinates")
                coordinate = (step["layer"], step["concurrency"])
                if coordinate not in expected or coordinate in seen:
                    raise ValueError(f"{index_path}: unexpected or repeated step coordinates")
                seen.add(coordinate)
                layer, concurrency = coordinate
                times = []
                for edge in EDGES:
                    stem = f"{layer}-c{concurrency}-{edge}"
                    if step.get(edge) != f"sample-{stem}.json":
                        raise ValueError(f"{index_path}: missing or mismatched {edge} sample path")
                    files = {
                        "index": index_path,
                        "sample": directory / step[edge],
                        "load": directory / f"load-{stem}.txt",
                        "MemAvailable": directory / f"memory-machine-{stem}.txt",
                        "memory.current": directory / f"memory-host-{stem}.txt",
                    }
                    taken_at = read_json(files["sample"]).get("taken_at_unix_ms")
                    if type(taken_at) is not int or taken_at <= 0:
                        raise ValueError(f"{files['sample']}: missing or invalid sample timestamp")
                    times.append(taken_at)
                    samples.append({
                        "run": run, "credential": credential, "repetition": repetition,
                        "layer": layer, "concurrency": concurrency, "edge": edge,
                        "source": index["source"], "taken_at_unix_ms": taken_at,
                        "source_files": {key: str(path.relative_to(root))
                                         for key, path in files.items()},
                        "values": read_metrics(files["load"], files["MemAvailable"],
                                               files["memory.current"]),
                    })
                if times[1] <= times[0]:
                    raise ValueError(f"{index_path}: after timestamp must follow before timestamp")
            if seen != expected:
                raise ValueError(f"{index_path}: incomplete layer/concurrency grid")
    if len(sources) != 1:
        raise ValueError(f"{root}: repetitions contain different source commits")
    return samples


def reduce_runs(before, after):
    roots = {"before": Path(before).resolve(), "after": Path(after).resolve()}
    if roots["before"] == roots["after"]:
        raise ValueError("The before and after run roots must be different")
    samples = [sample for run, root in roots.items() for sample in read_run(root, run)]
    groups = {}
    coordinates = ("run", "credential", "layer", "concurrency", "edge")
    for sample in samples:
        key = tuple(sample[field] for field in coordinates)
        groups.setdefault(key, []).append(sample)
    summary = []
    for key, matched in groups.items():
        summary.append(dict(zip(coordinates, key)) | {
            "repetitions": list(REPETITIONS),
            "statistics": {
                metric: {"min": min(values), "median": median(values), "max": max(values)}
                for metric in UNITS
                for values in [[sample["values"][metric] for sample in matched]]
            },
        })
    return {"schema": "wamn-fresh-auth-load-memory/v1",
            "run_roots": {key: str(path) for key, path in roots.items()},
            "units": UNITS, "samples": samples, "summary": summary}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True, help="Completed baseline run root")
    parser.add_argument("--after", type=Path, required=True, help="Completed candidate run root")
    args = parser.parse_args()
    try:
        result = reduce_runs(args.before, args.after)
    except ValueError as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
