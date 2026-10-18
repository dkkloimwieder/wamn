#!/usr/bin/env python3
"""Summarize perf CPU samples. Allocation figures are CPU time, not bytes."""
import collections
import json
import pathlib
import re
import sys

source = pathlib.Path(sys.argv[1])
self_time = collections.Counter()
inclusive = collections.Counter()
allocation_callers = collections.Counter()
split = collections.Counter()
samples = 0
total_period = 0
shallow = 0
unknown = 0
frame_pattern = re.compile(r'^\s+[0-9a-f]+\s+(.+)\s+\((.*)\)\s*$')
allocator = re.compile(r'(^|::)(__libc_|dl)?(malloc|free|cfree|calloc|realloc|memalign|aligned_alloc|posix_memalign)(@.*)?$|^__libc_malloc2$|^unlink_chunk(?:[.]|$)|^malloc_consolidate$|^_int_(malloc|free|realloc)|(^|::)(__rust_|__rdl_)(alloc|dealloc|realloc)|^(<?)(alloc::alloc::|alloc::raw_vec::|std::alloc::|std::sys::alloc::)')


def is_allocator(symbol):
    symbol = re.sub(r'\[[0-9a-f]{16}\]', '', symbol)
    symbol = re.sub(r'^wasm\[\d+\]::function\[\d+\]::', '', symbol)
    return allocator.search(symbol)


def category(symbol, dso):
    if 'perf-' in dso or symbol.startswith(('wasm[', 'wasm::')):
        return 'guest'
    if dso == '[unknown]':
        return 'unknown'
    if 'kernel' in dso:
        return 'kernel'
    return 'host'


def record(frames, period):
    global samples, shallow, unknown, total_period
    if not frames:
        return
    if not period:
        raise SystemExit("Decode cpu-clock:u with perf script -F +period")
    samples += 1
    total_period += period
    shallow += len(frames) < 3
    leaf = frames[0]
    self_time[leaf] += period
    split[category(*leaf)] += period
    unknown += any(symbol == '[unknown]' or category(symbol, dso) == 'unknown' for symbol, dso in frames)
    inclusive.update(dict.fromkeys(set(frames), period))
    if is_allocator(leaf[0]):
        caller = next((frame for frame in frames[1:] if not is_allocator(frame[0])), ('[unresolved caller]', '[unknown]'))
        allocation_callers[caller] += period


frames = []
period = 0
with source.open() as stream:
    for line in stream:
        header = re.search(r"\s(\d+)\s+cpu-clock:u:", line)
        if header:
            period = int(header[1])
        match = frame_pattern.match(line)
        if match:
            frames.append((re.sub(r'\+0x[0-9a-f]+$', '', match[1]), match[2]))
        elif not line.strip():
            record(frames, period)
            frames = []
record(frames, period)
if not samples:
    raise SystemExit('No perf call stacks were decoded')


def rows(counter, group=None, limit=30):
    return [dict(function=symbol, dso=dso, sampled_cpu_ms=count / 1_000_000, percent=100 * count / total_period)
            for (symbol, dso), count in counter.most_common()
            if group is None or category(symbol, dso) == group][:limit]


result = dict(samples=samples, sampled_cpu_ns=total_period, shallow_percent=100 * shallow / samples,
              stacks_with_unknown_percent=100 * unknown / samples,
              unresolved_self_percent=100 * sum(count for (symbol, _), count in self_time.items() if symbol == '[unknown]') / total_period,
              split_percent={group: 100 * count / total_period for group, count in split.items()},
              self_top30=rows(self_time), inclusive_top30=rows(inclusive),
              host_self_top30=rows(self_time, 'host'), guest_self_top30=rows(self_time, 'guest'),
              host_inclusive_top30=rows(inclusive, 'host'), guest_inclusive_top30=rows(inclusive, 'guest'),
              allocation_callers=rows(allocation_callers),
              allocation_method='CPU samples inside allocation/free functions, attributed to first non-allocator caller; not allocation counts or bytes')
pathlib.Path(sys.argv[2]).write_text(json.dumps(result, indent=2) + '\n')
