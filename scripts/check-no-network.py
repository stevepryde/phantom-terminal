#!/usr/bin/env python3
"""Validate Phantom's locked dependency graph and Rust socket authorities."""

import json
import os
import re
import sys
from pathlib import Path


# Any new direct dependency requires an explicit security review here. This is
# the fail-closed boundary for clients whose package names are not yet known.
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"

ALLOWED_DIRECT_EXTERNAL = {
    "phantom-app": {
        "arboard": "3.6.1", "egui": "0.35.0", "egui-wgpu": "0.35.0",
        "egui-winit": "0.35.0", "noyalib": "0.0.15", "objc2": "0.6.4",
        "objc2-app-kit": "0.3.2", "pollster": "1.0.1", "wgpu": "29.0.4",
        "winit": "0.30.13",
    },
    "phantom-core": {
        "directories": "6.0.0", "libc": "0.2.186", "noyalib": "0.0.15",
        "portable-pty": "0.9.0", "rusqlite": "0.40.1", "serde": "1.0.228",
        "serde_json": "1.0.150", "thiserror": "2.0.18",
    },
    "phantom-emu": {"alacritty_terminal": "0.26.0", "regex-syntax": "0.8.11"},
    "phantom-gfx": {
        "bytemuck": "1.25.1", "epaint_default_fonts": "0.35.0",
        "fontique": "0.11.0", "image": "0.25.10", "png": "0.18.1",
        "swash": "0.2.9", "unicode-width": "0.2.2", "wgpu": "29.0.4",
    },
}

ALLOWED_DIRECT_WORKSPACE = {
    "phantom-app": {"phantom-core", "phantom-emu", "phantom-gfx"},
    "phantom-core": set(),
    "phantom-emu": set(),
    "phantom-gfx": {"phantom-core", "phantom-emu"},
}

PROTOCOL_CLIENTS = {
    "attohttpc", "awc", "curl", "ehttp", "fastwebsockets", "h2", "hyper",
    "hyper-util", "isahc", "libssh2-sys", "minreq", "quinn", "quinn-proto",
    "quinn-udp", "reqwest", "ssh2", "surf", "tokio-tungstenite", "tonic",
    "tungstenite", "ureq", "webbrowser", "websocket",
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
    ("zbus", "5.19.0", CRATES_IO): {
        ("accesskit_unix", "0.21.1", CRATES_IO),
        ("atspi-common", "0.13.0", CRATES_IO),
        ("atspi-proxies", "0.13.0", CRATES_IO),
    },
    ("async-io", "2.6.0", CRATES_IO): {
        ("async-process", "2.5.0", CRATES_IO),
        ("async-signal", "0.2.14", CRATES_IO),
        ("zbus", "5.19.0", CRATES_IO),
    },
}

APPROVED_FEATURES = {
    "alacritty_terminal": {"default", "serde"},
    "arboard": {"core-graphics", "default", "image", "image-data", "windows-sys"},
    "bytemuck": {"aarch64_simd", "bytemuck_derive", "derive", "extern_crate_alloc", "min_const_generics"},
    "directories": set(),
    "egui": {"default", "default_fonts"},
    "egui-wgpu": {"default", "fragile-send-sync-non-atomic-wasm", "macos-window-resize-jitter-fix"},
    "egui-winit": {"accesskit"},
    "epaint_default_fonts": set(),
    "fontique": {"default", "fontconfig-dlopen", "std", "system"},
    "image": {"bmp", "png", "tiff", "webp"},
    "libc": {"default", "extra_traits", "std"},
    "noyalib": {"minimal", "std"},
    "objc2": {"alloc", "default", "relax-sign-encoding", "std"},
    "objc2-app-kit": {
        "NSImage", "NSPasteboard", "NSPasteboardItem", "NSResponder", "NSView",
        "NSWindow", "alloc", "bitflags", "objc2-core-graphics", "std",
    },
    "png": set(),
    "pollster": set(),
    "portable-pty": {"default"},
    "regex-syntax": {
        "default", "std", "unicode", "unicode-age", "unicode-bool", "unicode-case",
        "unicode-gencat", "unicode-perl", "unicode-script", "unicode-segment",
    },
    "rusqlite": {"bundled", "cache", "default", "ffi-sqlite-wasm-rs", "hashlink", "modern_sqlite"},
    "serde": {"alloc", "default", "derive", "rc", "serde_derive", "std"},
    "serde_json": {"default", "std"},
    "swash": {"default", "render", "scale", "std"},
    "thiserror": {"default", "std"},
    "unicode-width": {"cjk", "default"},
    "wgpu": {
        "default", "dx12", "fragile-send-sync-non-atomic-wasm", "gles", "metal",
        "parking_lot", "std", "vulkan", "web", "webgl", "webgpu", "wgpu-core",
        "wgsl",
    },
    "winit": {
        "ahash", "bytemuck", "memmap2", "percent-encoding", "rwh_06", "sctk",
        "sctk-adwaita", "wayland", "wayland-backend", "wayland-client",
        "wayland-csd-adwaita-notitle", "wayland-dlopen", "wayland-protocols",
        "wayland-protocols-plasma", "x11", "x11-dl", "x11rb",
    },
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
    re.compile(r"\bextern\s+crate\s+libc\s+as\b"),
    re.compile(r"\buse\s+(?:::)?libc\s+as\b"),
    re.compile(r"\buse\s+(?:::)?libc\s*::\s*\{[^}]*\bself\s+as\b", re.S),
    re.compile(r"\b(?:pub\s+)?use\s+(?:::)?libc\s*::\s*(?:\*|\{[^}]*\*)", re.S),
    re.compile(r"\blibc\s*::\s*(socket|connect|bind)\b"),
    re.compile(r"\blibc\s*::\s*\{[^}]*\b(socket|connect|bind)\b", re.S),
    re.compile(r"\b(?:unsafe\s+)?extern\s*\{[^}]*\b(socket|connect|bind)\s*\(", re.S),
    re.compile(r"\blink_name\b"),
    re.compile(r"#\s*\[\s*path\s*="),
    re.compile(r"\binclude\s*!\s*[({]"),
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


def identity(package):
    return package["name"], package["version"], package["source"]


def validate_graph(metadata):
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace_ids = set(metadata["workspace_members"])
    blocked = set()

    resolved_names = {packages_by_id[package_id]["name"] for package_id in nodes_by_id}
    for name in sorted(resolved_names & PROTOCOL_CLIENTS):
        blocked.add(f"resolved network client: {name}")
    exception_names = {exception[0] for exception in ACCESSKIT_EXCEPTIONS}
    for name in sorted((resolved_names & LOW_LEVEL_TRANSPORTS) - exception_names):
        blocked.add(f"resolved socket transport: {name}")

    for workspace_id in workspace_ids:
        workspace_name = packages_by_id[workspace_id]["name"]
        allowed_external = ALLOWED_DIRECT_EXTERNAL.get(workspace_name, {})
        allowed_workspace = ALLOWED_DIRECT_WORKSPACE.get(workspace_name, set())
        for dependency in nodes_by_id[workspace_id]["deps"]:
            package = packages_by_id[dependency["pkg"]]
            dependency_name = package["name"]
            approved = (
                dependency["pkg"] in workspace_ids
                and dependency_name in allowed_workspace
            ) or (
                package["source"] == CRATES_IO
                and allowed_external.get(dependency_name) == package["version"]
            )
            if not approved:
                blocked.add(
                    "unreviewed direct dependency identity: "
                    f"{workspace_name} -> {dependency_name} {package['version']} "
                    f"({package['source'] or 'path'})"
                )
            expected_features = APPROVED_FEATURES.get(dependency_name)
            if approved and dependency["pkg"] not in workspace_ids and expected_features is None:
                blocked.add(f"missing reviewed feature policy for {dependency_name}")
            elif expected_features is not None:
                actual_features = set(nodes_by_id[dependency["pkg"]]["features"])
                if actual_features != expected_features:
                    blocked.add(
                        f"unreviewed features for {dependency_name}: "
                        f"{sorted(actual_features)}"
                    )

    parents = {package_id: set() for package_id in nodes_by_id}
    for node in nodes_by_id.values():
        for dependency in node["deps"]:
            parents[dependency["pkg"]].add(node["id"])

    ids_by_name = {}
    for package_id in nodes_by_id:
        ids_by_name.setdefault(packages_by_id[package_id]["name"], set()).add(package_id)

    for name in exception_names:
        for package_id in ids_by_name.get(name, set()):
            package = packages_by_id[package_id]
            package_identity = identity(package)
            allowed_parents = ACCESSKIT_EXCEPTIONS.get(package_identity)
            if allowed_parents is None:
                blocked.add(
                    f"unreviewed AccessKit transport identity: {package_identity}"
                )
                allowed_parents = set()
            parent_identities = {
                identity(packages_by_id[parent]) for parent in parents[package_id]
            }
            for parent_identity in sorted(parent_identities - allowed_parents):
                blocked.add(f"unauthorized {name} parent identity: {parent_identity}")

            for workspace_id in workspace_ids:
                workspace_name = packages_by_id[workspace_id]["name"]
                for dependency in nodes_by_id[workspace_id]["deps"]:
                    if not reachable(nodes_by_id, dependency["pkg"], package_id):
                        continue
                    dependency_package = packages_by_id[dependency["pkg"]]
                    dependency_identity = identity(dependency_package)
                    if (workspace_name, dependency_identity) != (
                        "phantom-app",
                        ("egui-winit", "0.35.0", CRATES_IO),
                    ):
                        blocked.add(
                            f"unauthorized path to {name}: {workspace_name} -> "
                            f"{dependency_identity}"
                        )

            required_ancestors = {
                ("accesskit_winit", "0.32.2", CRATES_IO),
                ("accesskit_unix", "0.21.1", CRATES_IO),
            }
            if name == "async-io":
                required_ancestors.add(("zbus", "5.19.0", CRATES_IO))
            for ancestor_identity in required_ancestors:
                ancestor_ids = {
                    candidate_id
                    for candidate_id in ids_by_name.get(ancestor_identity[0], set())
                    if identity(packages_by_id[candidate_id]) == ancestor_identity
                }
                if not any(reachable(nodes_by_id, ancestor, package_id) for ancestor in ancestor_ids):
                    blocked.add(
                        f"{name} is outside approved {ancestor_identity} ancestry"
                    )
    return blocked


def validate_sources(metadata):
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    blocked = set()
    source_files = set()

    def collect_sources(root):
        if not root.is_dir():
            blocked.add(f"Rust source root is not a directory: {root}")
            return

        def record_walk_error(error):
            blocked.add(f"could not enumerate Rust sources under {root}: {error}")

        for current, directories, files in os.walk(
            root, onerror=record_walk_error, followlinks=False
        ):
            current_path = Path(current)
            for directory in list(directories):
                directory_path = current_path / directory
                if directory_path.is_symlink():
                    blocked.add(f"cannot scan symlinked Rust source directory: {directory_path}")
                    directories.remove(directory)
            for filename in files:
                if not filename.endswith(".rs"):
                    continue
                source_path = current_path / filename
                if source_path.is_symlink():
                    blocked.add(f"cannot scan symlinked Rust source file: {source_path}")
                else:
                    source_files.add(source_path)

    for workspace_id in metadata["workspace_members"]:
        package = packages_by_id[workspace_id]
        package_dir = Path(package["manifest_path"]).parent
        roots = {package_dir}
        for target in package["targets"]:
            target_path = Path(target["src_path"])
            if not target_path.is_file():
                blocked.add(f"Cargo target source is not a readable file: {target_path}")
            try:
                target_path.relative_to(package_dir)
            except ValueError:
                roots.add(target_path.parent)
        for root in roots:
            collect_sources(root)

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
