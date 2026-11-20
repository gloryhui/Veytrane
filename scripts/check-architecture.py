#!/usr/bin/env python3
"""Check source layers and Cargo production dependencies without building.

The source scan checks canonical imports. Cargo metadata checks internal package
ownership and the resolved production graph for each distribution profile.
"""
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LAYERS = {"core", "local", "api", "host", "adapters", "composition", "products"}
ALLOWED = {
    "core": {"core"},
    "local": {"local"},
    "api": {"core", "local", "api"},
    "composition": {"core", "composition"},
    "host": {"core", "local", "api", "host", "composition"},
    "adapters": {"core", "local", "api", "host", "adapters"},
    "products": {"core", "local", "api", "composition", "products"},
}
FOUNDATION = {"storage", "identity.rs", "instance_lock.rs", "nat", "net",
              "transport", "protocol", "discovery", "relay"}
WORKSPACE_DEPENDENCIES = {
    "veytrane-transport": set(),
    "veytrane-node": {"veytrane-transport"},
    "veytrane-file-policy": set(),
    "veytrane-files": {"veytrane-file-policy", "veytrane-node", "veytrane-transport"},
    "veytrane-speed": {"veytrane-node", "veytrane-transport"},
    "veytrane-tcp": {"veytrane-node", "veytrane-transport"},
    "veytrane-management": {"veytrane-transport"},
    "veytrane-http-auth": {"veytrane-transport"},
    "veytrane-web-management": {
        "veytrane-http-auth", "veytrane-management", "veytrane-transport"
    },
}
PROFILE_EXCLUSIONS = {
    "agent-base": {"veytrane-files", "veytrane-web-management", "axum", "gpui", "rusqlite"},
    "agent-base,agent-web": {"veytrane-files", "gpui", "rusqlite"},
    "agent-full": {"gpui", "rusqlite"},
    "desktop-client": {"veytrane-files", "veytrane-web-management", "axum", "rusqlite"},
    "live": {"veytrane-files", "veytrane-web-management", "gpui", "rusqlite"},
    "sub2api-client,sub2api-gateway": {
        "veytrane-files", "veytrane-web-management", "axum", "gpui", "rusqlite"
    },
    "standalone": {"veytrane-management", "veytrane-web-management", "gpui"},
}


def violations(root):
    errors = []
    for path in sorted((root / "src").rglob("*.rs")):
        relative = path.relative_to(root)
        parts = relative.parts
        # Keep line numbers stable, ignore line comments/doc comments.
        lines = [line.split("//", 1)[0] for line in path.read_text(encoding="utf-8").splitlines()]
        code = "\n".join(lines)

        def reject(pattern, reason):
            for match in re.finditer(pattern, code):
                line = code.count("\n", 0, match.start()) + 1
                errors.append(f"{relative}:{line}: {reason}")

        if parts[1] == "node" and len(parts) > 3:
            layer = parts[2]
            if layer not in LAYERS:
                errors.append(f"{relative}: unknown node layer {layer}")
                continue
            reject(r"\bdesktop\s*::", "node must not depend on desktop presentation")
            for match in re.finditer(r"\bcrate::node::(\w+)", code):
                dependency = match[1]
                if dependency not in ALLOWED[layer]:
                    line = code.count("\n", 0, match.start()) + 1
                    errors.append(f"{relative}:{line}: {layer} -> {dependency} is forbidden; use the owning layer")
            reject(r"\bcrate::node::\s*\{", "use explicit layer imports")
        if parts[1] in FOUNDATION:
            reject(r"\b(?:node|desktop)\s*::", "foundation must not depend on node/desktop")
        if parts[1:3] == ("desktop", "agent_shell") or relative.as_posix() == "src/desktop/agent_shell.rs":
            reject(r"\blegacy\s*::|\bDesktopShell\b", "Agent desktop must not borrow the embedded legacy runtime")
            reject(r"crate::node::(?:host|products)::", "Agent desktop must use management contracts, not a runtime owner")
        if parts[1:3] in (("desktop", "ui"), ("desktop", "platform")):
            reject(r"\blegacy\s*::|\bDesktopShell\b|\bAgentShell\b", "shared UI/native primitives must not depend on shell owners")
    return errors


def metadata(root, *arguments):
    output = subprocess.check_output(
        ["cargo", "metadata", "--locked", "--offline", "--format-version", "1", *arguments],
        cwd=root,
        text=True,
    )
    return json.loads(output)


def cargo_violations(root):
    errors = []
    declared = metadata(root, "--no-deps")
    internal = {package["name"] for package in declared["packages"]}
    for package in declared["packages"]:
        name = package["name"]
        if name == "p2p_file":
            continue
        if name not in WORKSPACE_DEPENDENCIES:
            errors.append(f"{name}: missing workspace dependency policy")
            continue
        actual = {
            dependency["name"] for dependency in package["dependencies"]
            if dependency["kind"] is None and dependency["name"] in internal
        }
        for dependency in sorted(actual - WORKSPACE_DEPENDENCIES[name]):
            errors.append(f"{name} -> {dependency}: forbidden production dependency")

    for features, excluded in PROFILE_EXCLUSIONS.items():
        command = [
            "cargo", "tree", "--locked", "--offline", "-p", "p2p_file",
            "--no-default-features", "--features", features, "-e", "normal",
            "--prefix", "none", "--format", "{p}",
        ]
        output = subprocess.check_output(command, cwd=root, text=True)
        dependencies = {line.split()[0] for line in output.splitlines() if line.strip()}
        for dependency in sorted(dependencies & excluded):
            errors.append(f"{features} links forbidden production dependency {dependency}")
    return errors


if __name__ == "__main__":
    errors = violations(ROOT) + cargo_violations(ROOT)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        sys.exit(1)
    print("Architecture boundaries OK: source layers and Cargo production profiles.")
