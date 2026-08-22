#!/usr/bin/env python3
"""Validate Phantom's locked dependency graph and Rust socket authorities."""

import json
import re
import sys
from pathlib import Path


# Any new direct dependency requires an explicit security review here. This is
# the fail-closed boundary for clients whose package names are not yet known.
ALLOWED_DIRECT = {
    "phantom-app": {
        "arboard", "egui", "egui-wgpu", "egui-winit", "noyalib", "objc2",
        "objc2-app-kit", "phantom-core", "phantom-emu", "phantom-gfx",
        "pollster", "wgpu", "winit",
    },
    "phantom-core": {
        "directories", "libc", "noyalib", "portable-pty", "rusqlite", "serde",
        "serde_json", "thiserror",
    },
    "phantom-emu": {"alacritty_terminal", "regex-syntax"},
    "phantom-gfx": {
        "bytemuck", "epaint_default_fonts", "fontique", "image", "phantom-core",
        "phantom-emu", "png", "swash", "unicode-width", "wgpu",
    },
}

PROTOCOL_CLIENTS = {
    "attohttpc", "awc", "curl", "ehttp", "fastwebsockets", "h2", "hyper",
    "hyper-util", "isahc", "libssh2-sys", "minreq", "quinn", "quinn-proto",
    "quinn-udp", "reqwest", "ssh2", "surf", "tokio-tungstenite", "tonic",
    "tungstenite", "ureq", "websocket",
}

LOW_LEVEL_TRANSPORTS = {
    "async-io", "async-net", "async-std", "mio", "nng", "socket2", "tokio",
    "zbus", "zmq",
}

# AccessKit's local Unix D-Bus backend is the sole low-level exception. Pin its
# resolved versions and permitted immediate parents so lockfile or ancestry
# changes require review. Runtime initialization separately rejects non-unix
# DBUS_SESSION_BUS_ADDRESS values.
ACCESSKIT_EXCEPTIONS = {
    "zbus": ("5.19.0", {"accesskit_unix", "atspi-common", "atspi-proxies"}),
    "async-io": ("2.6.0", {"async-process", "async-signal", "zbus"}),
}

SOCKET_PATTERNS = [
    re.compile(r"\bstd\s*::\s*net\s*::\s*(TcpStream|TcpListener|UdpSocket)\b"),
    re.compile(r"\bnet\s*::\s*(TcpStream|TcpListener|UdpSocket)\b"),
    re.compile(r"\bnet\s*::\s*\{[^}]*\b(TcpStream|TcpListener|UdpSocket)\b", re.S),
    re.compile(r"\buse\s+std\s*::\s*net\s*(?:;|\bas\b)"),
    re.compile(r"\buse\s+std\s*::\s*\{[^}]*\bnet\s*(?:,|\bas\b|\})", re.S),
    re.compile(r"\b(TcpStream|TcpListener|UdpSocket)\s*::\s*(connect|bind)\b"),
    re.compile(r"\b(tokio|async_std|smol|mio)\s*::\s*net\b"),
    re.compile(r"\bsocket2\s*::"),
    re.compile(r"\bnix\s*::\s*sys\s*::\s*socket\b"),
    re.compile(r"\b(?:use|extern\s+crate)\s+libc\b"),
    re.compile(r"\blibc\s*::\s*(socket|connect|bind)\b"),
    re.compile(r"\blibc\s*::\s*\{[^}]*\b(socket|connect|bind)\b", re.S),
]


def strip_comments_and_literals(source):
    """Replace Rust comments and string/char contents while preserving lines."""
    result = list(source)
    i = 0
    length = len(source)
    while i < length:
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = length if end < 0 else end
            for offset in range(i, end):
                result[offset] = " "
            i = end
            continue
        if source.startswith("/*", i):
            start = i
            depth = 1
            i += 2
            while i < length and depth:
                if source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            for offset in range(start, i):
                if result[offset] != "\n":
                    result[offset] = " "
            continue

        raw = re.match(r'(?:b|c)?r(#{0,255})"', source[i:])
        if raw:
            start = i
            hashes = raw.group(1)
            i += raw.end()
            end_marker = '"' + hashes
            end = source.find(end_marker, i)
            i = length if end < 0 else end + len(end_marker)
            for offset in range(start, i):
                if result[offset] != "\n":
                    result[offset] = " "
            continue

        prefix = 1 if source.startswith(('b"', 'c"'), i) else 0
        if i + prefix < length and source[i + prefix] == '"':
            start = i
            i += prefix + 1
            escaped = False
            while i < length:
                char = source[i]
                i += 1
                if char == '"' and not escaped:
                    break
                escaped = char == "\\" and not escaped
                if char != "\\":
                    escaped = False
            for offset in range(start, i):
                if result[offset] != "\n":
                    result[offset] = " "
            continue

        char_literal = re.match(r"(?:b)?'(?:\\.|[^\\'\n])'", source[i:])
        if char_literal:
            end = i + char_literal.end()
            for offset in range(i, end):
                result[offset] = " "
            i = end
            continue
        i += 1
    return "".join(result)


def reachable(nodes_by_id, start, target):
    pending = [start]
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id == target:
            return True
        if package_id in visited:
            continue
        visited.add(package_id)
        pending.extend(dep["pkg"] for dep in nodes_by_id[package_id]["deps"])
    return False


def validate_graph(metadata):
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace_ids = set(metadata["workspace_members"])
    blocked = set()

    resolved_names = {packages_by_id[package_id]["name"] for package_id in nodes_by_id}
    for name in sorted(resolved_names & PROTOCOL_CLIENTS):
        blocked.add(f"resolved network client: {name}")
    for name in sorted((resolved_names & LOW_LEVEL_TRANSPORTS) - ACCESSKIT_EXCEPTIONS.keys()):
        blocked.add(f"resolved socket transport: {name}")

    for workspace_id in workspace_ids:
        workspace_name = packages_by_id[workspace_id]["name"]
        allowed = ALLOWED_DIRECT.get(workspace_name, set())
        for dependency in nodes_by_id[workspace_id]["deps"]:
            dependency_name = packages_by_id[dependency["pkg"]]["name"]
            if dependency_name not in allowed:
                blocked.add(
                    f"unreviewed direct dependency: {workspace_name} -> {dependency_name}"
                )

    parents = {package_id: set() for package_id in nodes_by_id}
    for node in nodes_by_id.values():
        for dependency in node["deps"]:
            parents[dependency["pkg"]].add(node["id"])

    ids_by_name = {}
    for package_id in nodes_by_id:
        ids_by_name.setdefault(packages_by_id[package_id]["name"], set()).add(package_id)

    for name, (version, allowed_parents) in ACCESSKIT_EXCEPTIONS.items():
        for package_id in ids_by_name.get(name, set()):
            package = packages_by_id[package_id]
            if package["version"] != version:
                blocked.add(f"unreviewed AccessKit transport version: {name} {package['version']}")
            parent_names = {packages_by_id[parent]["name"] for parent in parents[package_id]}
            for parent_name in sorted(parent_names - allowed_parents):
                blocked.add(f"unauthorized {name} parent: {parent_name}")

            for workspace_id in workspace_ids:
                workspace_name = packages_by_id[workspace_id]["name"]
                for dependency in nodes_by_id[workspace_id]["deps"]:
                    if not reachable(nodes_by_id, dependency["pkg"], package_id):
                        continue
                    dependency_name = packages_by_id[dependency["pkg"]]["name"]
                    if (workspace_name, dependency_name) != ("phantom-app", "egui-winit"):
                        blocked.add(
                            f"unauthorized path to {name}: {workspace_name} -> {dependency_name}"
                        )

            required_ancestors = {"accesskit_winit", "accesskit_unix"}
            if name == "async-io":
                required_ancestors.add("zbus")
            for ancestor_name in required_ancestors:
                ancestor_ids = ids_by_name.get(ancestor_name, set())
                if not any(reachable(nodes_by_id, ancestor, package_id) for ancestor in ancestor_ids):
                    blocked.add(f"{name} is outside approved {ancestor_name} ancestry")
    return blocked


def validate_sources(metadata):
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    blocked = set()
    source_files = set()
    for workspace_id in metadata["workspace_members"]:
        package_dir = Path(packages_by_id[workspace_id]["manifest_path"]).parent
        try:
            source_files.update(package_dir.rglob("*.rs"))
        except OSError as error:
            blocked.add(f"could not enumerate Rust sources under {package_dir}: {error}")

    for source_file in sorted(source_files):
        try:
            source = source_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            blocked.add(f"could not read Rust source {source_file}: {error}")
            continue
        code = strip_comments_and_literals(source)
        for pattern in SOCKET_PATTERNS:
            match = pattern.search(code)
            if match:
                line = code.count("\n", 0, match.start()) + 1
                blocked.add(f"direct socket API: {source_file}:{line}")
                break
    return blocked


def main():
    with open(sys.argv[1], encoding="utf-8") as metadata_file:
        metadata = json.load(metadata_file)
    blocked = validate_graph(metadata) | validate_sources(metadata)
    for finding in sorted(blocked):
        print(f"  ✗ {finding}", file=sys.stderr)
    return bool(blocked)


if __name__ == "__main__":
    sys.exit(main())
