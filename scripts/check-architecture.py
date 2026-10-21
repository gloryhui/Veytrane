#!/usr/bin/env python3
"""Check explicit Rust layer dependencies without building or touching node state.

This is a source-level guard, not a Rust parser. Compilation remains responsible
for name resolution; code in these layers uses canonical crate-qualified imports
so the same guard covers tests and platform-specific branches on every CI host.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
LAYERS = {"core", "local", "api", "host", "adapters", "products"}
ALLOWED = {
    "core": {"core"},
    "local": {"local"},
    "api": {"core", "local", "api"},
    "host": {"core", "local", "api", "host"},
    "adapters": {"core", "local", "api", "host", "adapters"},
    "products": {"core", "local", "api", "host", "products"},
}
FOUNDATION = {"storage", "identity.rs", "instance_lock.rs", "nat", "net",
              "transport", "protocol", "discovery", "relay"}


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
            reject(r"\bcrate::node::\s*\{", "flat compatibility imports are for external consumers only")
        if parts[1] in FOUNDATION:
            reject(r"\b(?:node|desktop)\s*::", "foundation must not depend on node/desktop")
        if parts[1:3] == ("desktop", "agent_shell") or relative.as_posix() == "src/desktop/agent_shell.rs":
            reject(r"\blegacy\s*::|\bDesktopShell\b", "Agent desktop must not borrow the embedded legacy runtime")
            reject(r"crate::node::(?:host|products)::", "Agent desktop must use management contracts, not a runtime owner")
        if parts[1:3] in (("desktop", "ui"), ("desktop", "platform")):
            reject(r"\blegacy\s*::|\bDesktopShell\b|\bAgentShell\b", "shared UI/native primitives must not depend on shell owners")
    return errors


if __name__ == "__main__":
    errors = violations(ROOT)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        sys.exit(1)
    print("Architecture boundaries OK: core, local, API, host, adapters, products and desktop.")
