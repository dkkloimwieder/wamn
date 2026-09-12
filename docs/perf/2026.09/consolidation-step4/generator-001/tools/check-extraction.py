from pathlib import Path
import subprocess, json, hashlib, re

tree = Path("/home/kaalin/.cache/wamn-lanes/consolidation-generator-20260912")
base = "510e4fbf22d8ab321ef3cff1eabc69cf487a3cce"
relative = "crates/schema/generator/src/generate.rs"
old = subprocess.check_output(["git", "show", f"{base}:{relative}"], cwd=tree).decode()

def functions(s):
    a = list(s)
    i = 0
    while i < len(s):
        start = i
        if s.startswith("//", i):
            i = s.find("\n", i)
            if i < 0: i = len(s)
        elif s.startswith("/*", i):
            depth = 1
            i += 2
            while depth:
                if s.startswith("/*", i): depth += 1; i += 2
                elif s.startswith("*/", i): depth -= 1; i += 2
                else: i += 1
        elif (m := re.match(r'(?:b|c)?r(#+)?"', s[i:])):
            suffix = '"' + (m[1] or '')
            i = s.index(suffix, i + m.end()) + len(suffix)
        elif s[i] == '"':
            i += 1
            while i < len(s):
                if s[i] == "\\": i += 2
                elif s[i] == '"': i += 1; break
                else: i += 1
        elif (m := re.match(r"'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'", s[i:])):
            i += len(m[0])
        else:
            i += 1
            continue
        for j in range(start, i):
            if a[j] != "\n": a[j] = " "
    masked = "".join(a)
    result = {}
    for m in re.finditer(r"^(?:pub(?:\([^)]*\))? )?fn (\w+)", masked, re.M):
        assert masked[:m.start()].count("{") == masked[:m.start()].count("}")
        opening = masked.index("{", m.end())
        level, end = 1, opening + 1
        while level:
            if masked[end] == "{": level += 1
            if masked[end] == "}": level -= 1
            end += 1
        if s[end:end + 1] == "\n": end += 1
        start = m.start()
        lines = s[:start].splitlines(True)
        if lines and lines[-1].strip() == ")]":
            while lines:
                line = lines.pop()
                start -= len(line)
                if line.startswith("#["): break
        while lines and (lines[-1].startswith("///") or lines[-1].startswith("//")):
            start -= len(lines.pop())
        result[m[1]] = {"start": start, "fn_start": m.start(), "body_start": opening, "end": end}
    return result

def remainder(s, spans):
    for row in reversed(list(spans.values())):
        s = s[:row["start"]] + s[row["end"]:]
    s = s[s.index("const POSTGRES_INTERFACE:"):]
    return "\n".join(line for line in s.splitlines() if line.strip())

original = functions(old)
records, files, parent = [], {}, None
for group in ("parent", "validation", "contracts", "rust"):
    path = relative if group == "parent" else f"crates/schema/generator/src/generate/{group}.rs"
    current = (tree / path).read_text()
    spans = functions(current)
    for name, span in spans.items():
        before = original[name]
        body = old[before["body_start"]:before["end"]]
        assert body == current[span["body_start"]:span["end"]], (name, "body")
        a = old[before["fn_start"]:before["body_start"]]
        b = current[span["fn_start"]:span["body_start"]].removeprefix("pub(super) ")
        assert re.sub(r"\s+", "", a).replace(",)", ")") == re.sub(r"\s+", "", b).replace(",)", ")"), (name, "signature")
        assert old[before["start"]:before["fn_start"]] == current[span["start"]:span["fn_start"]], (name, "comments")
        records.append({"name": name, "path": path, "body_sha256": hashlib.sha256(body.encode()).hexdigest(), "body_equal": True, "signature_equal_except_private_visibility_whitespace_and_trailing_comma": True, "comments_equal": True})
    if group == "parent": parent = remainder(current, spans)
    files[path] = {"sha256": hashlib.sha256((tree / path).read_bytes()).hexdigest(), "mode": oct((tree / path).stat().st_mode & 0o777), "lines": len(current.splitlines()), "functions": len(spans)}
assert len(records) == len(original) == len({row["name"] for row in records}) == 101
assert parent == remainder(old, original)
lib = "crates/schema/generator/src/lib.rs"
assert subprocess.check_output(["git", "show", f"{base}:{lib}"], cwd=tree) == (tree / lib).read_bytes()
result = {"base_source": base, "original_source_sha256": hashlib.sha256(old.encode()).hexdigest(), "function_count": len(records), "functions": records, "parent_non_function_declarations_equal_ignoring_empty_lines": True, "lib_rs_unchanged": True, "files": files}
output = Path(__file__).resolve().parents[1] / "extraction.json"
output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
print(json.dumps({key: value for key, value in result.items() if key != "functions"}, indent=2))
