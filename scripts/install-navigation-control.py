#!/usr/bin/env python3
"""Prepare integrated p2p-agent navigation configuration and private page.

This compatibility installer does not start Python HTTP services, register
launchd/systemd/scheduled-task entries, edit Nginx, modify Agent identity, or
restart an Agent. --replace, --no-autostart, and --foreground are accepted as
historical flags and never start an Agent.
"""
import argparse
import copy
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import uuid

from navigation_platform import load_config, secure_private_file


PERSISTED_FIELDS = frozenset((
    "schema_version", "local_name", "installed", "page", "state_dir", "http",
    "remote", "peers", "lifecycle", "agent_web",
))
INTEGRATED_PORT = 3001


def run(argv, check=True):
    kwargs = {}
    if os.name == "nt":
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    return subprocess.run(
        [str(value) for value in argv],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=check,
        timeout=110,
        **kwargs,
    )


def atomic_private(path, value):
    """Apply private permissions before writing, then atomically replace."""
    path = Path(path)
    ensure_directory(path.parent)
    if path.is_symlink():
        raise RuntimeError(f"Private file target must not be a symlink: {path}")
    temporary = path.parent / (".navigation-" + uuid.uuid4().hex)
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        secure_private_file(temporary)
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            fd = -1
            stream.write(value if isinstance(value, str) else json.dumps(
                value, ensure_ascii=False, indent=2))
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        secure_private_file(path)
    finally:
        if fd >= 0:
            os.close(fd)


def canonical_absolute(path):
    value = Path(path).expanduser()
    absolute = Path(os.path.abspath(value))
    try:
        return absolute.resolve(strict=False) == absolute, absolute
    except OSError:
        return False, absolute


def require_path(path, *, directory=False):
    canonical, absolute = canonical_absolute(path)
    if not canonical:
        raise RuntimeError(f"Expected a canonical non-symlink path: {path}")
    if directory:
        if not absolute.is_dir() or absolute.is_symlink():
            raise RuntimeError(f"Expected an existing directory: {absolute}")
    elif not absolute.is_file() or absolute.is_symlink():
        raise RuntimeError(f"Expected an existing regular file: {absolute}")
    if os.name != "nt" and absolute.stat().st_uid != os.getuid():
        raise RuntimeError(f"Expected current-user ownership: {absolute}")
    return absolute


def ensure_directory(path):
    """Create a missing current-user directory without traversing symlinks."""
    _, absolute = canonical_absolute(path)
    missing = []
    current = absolute
    while not current.exists():
        missing.append(current.name)
        current = current.parent
    require_path(current, directory=True)
    for name in reversed(missing):
        current = current / name
        current.mkdir(mode=0o700)
        require_path(current, directory=True)
    return require_path(absolute, directory=True)


def effective_config(config, args):
    value = copy.deepcopy(config)
    for field, argument in (
            ("installed", args.installed),
            ("page", args.page),
            ("state_dir", args.state_dir)):
        if argument is not None:
            value[field] = str(Path(argument).expanduser().resolve(strict=False))
    if args.ssh_host is not None:
        remote = value.get("remote")
        if not isinstance(remote, dict):
            raise ValueError("--ssh-host requires a non-null remote configuration")
        remote["ssh_host"] = args.ssh_host
    return value


def validate_config(config):
    host_platform = {"Darwin": "macos", "Windows": "windows", "Linux": "linux"}.get(
        platform.system())
    if host_platform is None:
        raise RuntimeError("unsupported host platform: " + platform.system())
    if config.get("platform") != host_platform:
        raise RuntimeError(
            f"configuration is for {config.get('platform')}, not {host_platform}")
    if config.get("http") != {"port": INTEGRATED_PORT, "public_port": INTEGRATED_PORT}:
        raise RuntimeError("integrated navigation requires port and public_port 3001")
    lifecycle = config.get("lifecycle")
    expected_kind = {"macos": "launchd", "windows": "scheduled-task",
                     "linux": "systemd"}[host_platform]
    if (not isinstance(lifecycle, dict) or
            lifecycle.get("kind") not in (expected_kind, "none")):
        raise RuntimeError("Agent lifecycle in the navigation configuration is invalid")

    installed = require_path(config["installed"])
    try:
        metadata = json.loads(installed.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RuntimeError("installed Agent metadata is unreadable") from error
    if not isinstance(metadata, dict) or not {"bin_dir", "data_dir"} <= metadata.keys():
        raise RuntimeError("installed Agent metadata is not valid")
    data_dir = require_path(metadata["data_dir"], directory=True)
    navigation_root = ensure_directory(data_dir.parent / "navigation")

    page = canonical_absolute(config["page"])[1]
    if page.parent != navigation_root:
        raise RuntimeError(
            f"navigation page must be directly inside {navigation_root}: {page}")
    if page.exists():
        page = require_path(page)
    if page == navigation_root / "config.json":
        raise RuntimeError("navigation page cannot replace config.json")

    state_dir = canonical_absolute(config["state_dir"])[1]
    try:
        state_dir.relative_to(navigation_root)
    except ValueError as error:
        raise RuntimeError(
            f"navigation state must remain inside {navigation_root}: {state_dir}") from error
    ensure_directory(state_dir)

    config_path = navigation_root / "config.json"
    if config_path.exists():
        require_path(config_path)
    config["page"] = str(page)
    config["state_dir"] = str(state_dir)
    config["navigation_root"] = str(navigation_root)
    config["config_file"] = str(config_path)
    return {
        "platform": host_platform,
        "installed": installed,
        "data_dir": data_dir,
        "navigation_root": navigation_root,
        "page": page,
        "state_dir": state_dir,
        "config_path": config_path,
    }


def persist_config(config_path, config):
    payload = {key: copy.deepcopy(config[key]) for key in PERSISTED_FIELDS if key in config}
    atomic_private(config_path, payload)
    return Path(config_path)


def page_matches_host(config, page):
    page = Path(page)
    if not page.is_file() or page.is_symlink():
        return False
    try:
        html = page.read_text(encoding="utf-8")
        match = re.search(
            r'<script id="navigation-data" type="application/json">([\s\S]*?)</script>',
            html,
        )
        if not match:
            return False
        data = json.loads(match.group(1))
        return (isinstance(data.get("host"), dict) and
                data["host"].get("platform") == config["platform"])
    except (OSError, UnicodeError, ValueError, AttributeError):
        return False


def fallback_local_page(here, config, page):
    """Create a usable local-only page when one-shot discovery is unavailable."""
    template = here.parent / "service-navigation.html"
    spec = importlib.util.spec_from_file_location(
        "update_service_navigation", here / "update-service-navigation.py")
    if spec is None or spec.loader is None:
        raise RuntimeError("Unable to load the navigation page generator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    host = module.navigation_host(config)
    data = {
        "updatedAt": __import__("datetime").datetime.now().astimezone().isoformat(
            timespec="seconds"),
        "host": host,
        "services": [module.local_admin_service(config, host)],
    }
    rendered = module.render_data(template.read_text(encoding="utf-8"), data)
    atomic_private(page, rendered)


def generate_page(here, config, config_path):
    """Run the one-shot Python synchronizer and preserve a usable private page."""
    page = Path(config["page"])
    template = here.parent / "service-navigation.html"
    is_template_copy = (
        page.is_file() and template.is_file() and
        page.read_bytes() == template.read_bytes()
    )
    prior = (page.read_bytes()
             if page_matches_host(config, page) and not is_template_copy else None)
    result = run([
        sys.executable,
        here / "update-service-navigation.py",
        "--navigation-config", config_path,
        "--output", page,
    ], check=False)
    if result.returncode == 0 and page_matches_host(config, page):
        return "remote-or-authorized-catalog"
    if prior is not None:
        if not page_matches_host(config, page) or page.read_bytes() != prior:
            atomic_private(page, prior)
        return "preserved-existing"
    fallback_local_page(here, config, page)
    return "local-only-fallback"


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, help="source platform configuration")
    parser.add_argument(
        "--replace", action="store_true",
        help="compatibility flag; replacement never starts or restarts the Agent")
    parser.add_argument(
        "--no-autostart", "--foreground", dest="no_autostart", action="store_true",
        help="compatibility flag; this installer never registers or starts a host")
    parser.add_argument("--installed", type=Path, help="installed Agent metadata override")
    parser.add_argument("--page", type=Path, help="private navigation page inside data_dir/navigation")
    parser.add_argument("--state-dir", type=Path, help="private state inside data_dir/navigation")
    parser.add_argument("--ssh-host", help="independent remote SSH host override")
    return parser.parse_args(argv)


def prepare(args):
    config = effective_config(load_config(args.config), args)
    locations = validate_config(config)
    config_path = persist_config(locations["config_path"], config)
    here = Path(__file__).resolve().parent
    page_source = generate_page(here, config, config_path)
    return {
        "integrated": True,
        "prepared": True,
        "requires_agent_restart": True,
        "agent_started": False,
        "agent_restarted": False,
        "config": str(config_path),
        "page": str(locations["page"]),
        "page_source": page_source,
        "port": INTEGRATED_PORT,
        "compatibility": {
            "replace": args.replace,
            "no_autostart_or_foreground": args.no_autostart,
        },
    }


def main(argv=None):
    if sys.version_info < (3, 10):
        raise SystemExit("Python 3.10 or newer is required")
    args = parse_args(argv)
    try:
        result = prepare(args)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error)) from error
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
