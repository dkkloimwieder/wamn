#!/usr/bin/env python3
"""Exercise matching and missing contracts for each generated constraint kind."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--tree", type=Path, required=True)
parser.add_argument("--evidence", type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence.resolve(strict=True)
generated = tree / "packages/receiving/generated/wamn/purchase_order.rs"
contract = tree / "packages/receiving/generated/contracts/purchase_order/update.errors.json"
original_generated = generated.read_bytes()
original_contract = contract.read_bytes()
original = {str(path.relative_to(tree)): hashlib.sha256(data).hexdigest()
            for path, data in [(generated, original_generated), (contract, original_contract)]}
command = ["cargo", "test", "--manifest-path", "components/Cargo.toml", "-p",
           "wamn-receiving-data-access", "--lib", "--locked", "--offline",
           "error::tests::the_hand_copied_vocabulary_agrees_with_the_generated_contracts",
           "--", "--exact"]
results = []

def run(label, expected, literal):
    destination = evidence / label
    result = subprocess.run(["python3", str(evidence / "tools/capture.py"), "--tree", str(tree),
                             "--evidence-dir", str(destination), "--", *command], check=False)
    output = (destination / "stdout.log").read_text()
    expected_summary = "1 passed; 0 failed" if expected == 0 else "0 passed; 1 failed"
    assert result.returncode == expected, (label, result.returncode, expected)
    assert expected_summary in output, (label, output)
    if expected != 0:
        assert f"the generated {literal} slice and contract disagree" in output, (label, output)
    results.append({"label": label, "exit_code": result.returncode})

try:
    for name, literal in [("UNIQUE", "unique_violation"),
                          ("FOREIGN_KEY", "foreign_key_violation"),
                          ("CHECK", "check_violation"),
                          ("EXCLUSION", "exclusion_violation")]:
        anchor = f"pub const UPDATE_{name}_CONSTRAINTS: &[&str] = &[];".encode()
        assert original_generated.count(anchor) == 1, name
        constraint = f"probe_{literal}"
        replacement = f'pub const UPDATE_{name}_CONSTRAINTS: &[&str] = &["{constraint}"];'.encode()
        generated.write_bytes(original_generated.replace(anchor, replacement))
        declared = json.loads(original_contract)
        declared["cases"].append({"literal": literal, "from": literal,
                                  "constraint": constraint, "detail": {}})
        contract.write_text(json.dumps(declared, separators=(",", ":")))
        run(f"{literal}-matching-001", 0, literal)
        contract.write_bytes(original_contract)
        run(f"{literal}-undeclared-001", 101, literal)
        generated.write_bytes(original_generated)
finally:
    generated.write_bytes(original_generated)
    contract.write_bytes(original_contract)
    restored = {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in [generated, contract]}
    (evidence / "constraint-controls.json").write_text(
        json.dumps({"original_sha256": original, "restored_sha256": restored,
                    "restored_exactly": restored == original, "runs": results}, indent=2) + "\n")
    assert restored == original
run("restored-001", 0, "")
