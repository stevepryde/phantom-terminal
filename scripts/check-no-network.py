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

EXPECTED_WORKSPACE = set(ALLOWED_DIRECT_EXTERNAL)

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

TOKEN_RE = re.compile(r"r#[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*|::|->|=>|[^\s]")
IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SOCKET_TYPES = {"TcpStream", "TcpListener", "UdpSocket"}
SOCKET_FUNCTIONS = {"socket", "connect", "bind", "syscall", "dlopen", "dlsym"}
ALLOWED_MACROS = {
    "assert", "assert_eq", "assert_ne", "cfg", "env", "eprintln", "format",
    "harness_or_skip", "include_bytes", "include_str", "json", "macro_rules",
    "matches", "panic", "params", "print", "println", "vec", "vertex_attr_array",
    "write",
}


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
            if depth:
                raise ValueError("unterminated block comment")
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
            if end < 0:
                raise ValueError("unterminated raw string")
            i = end + len(end_marker)
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
            else:
                raise ValueError("unterminated string")
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


def rust_tokens(source):
    tokens = []
    for match in TOKEN_RE.finditer(source):
        value = match.group(0)
        if value.startswith("r#"):
            value = value[2:]
        tokens.append((value, match.start()))
    return tokens


def sequence_at(values, start, sequence):
    return values[start : start + len(sequence)] == list(sequence)


def find_sequence(values, sequence):
    for index in range(len(values) - len(sequence) + 1):
        if sequence_at(values, index, sequence):
            return index
    return None


def source_violation(tokens):
    values = [value for value, _ in tokens]

    delimiters = {"(": ")", "[": "]", "{": "}"}
    stack = []
    for index, value in enumerate(values):
        if value in delimiters:
            stack.append((delimiters[value], index))
        elif value in delimiters.values():
            if not stack or stack[-1][0] != value:
                return index
            stack.pop()
    if stack:
        return stack[-1][1]

    for index in range(len(values) - 2):
        if values[index + 1] != "!" or values[index + 2] not in {"(", "[", "{"}:
            continue
        macro_name = values[index]
        if not IDENTIFIER_RE.fullmatch(macro_name) or macro_name in {"if", "while"}:
            continue
        if macro_name not in ALLOWED_MACROS:
            return index

    for index in range(len(values) - 1):
        if values[index : index + 2] != ["#", "["]:
            continue
        depth = 1
        end = index + 2
        while end < len(values) and depth:
            if values[end] == "[":
                depth += 1
            elif values[end] == "]":
                depth -= 1
            end += 1
        attribute = values[index + 2 : end - 1]
        authorities = {"path", "link", "link_name", "link_ordinal"}
        if attribute and (
            attribute[0] in authorities
            or (attribute[0] == "cfg_attr" and any(item in authorities for item in attribute))
        ):
            return index

    direct_sequences = [
        ("std", "::", "net", "::", "TcpStream"),
        ("std", "::", "net", "::", "TcpListener"),
        ("std", "::", "net", "::", "UdpSocket"),
        ("net", "::", "TcpStream"),
        ("net", "::", "TcpListener"),
        ("net", "::", "UdpSocket"),
        ("tokio", "::", "net"),
        ("async_std", "::", "net"),
        ("smol", "::", "net"),
        ("mio", "::", "net"),
        ("socket2", "::"),
        ("nix", "::", "sys", "::", "socket"),
        ("libc", "::", "socket"),
        ("libc", "::", "connect"),
        ("libc", "::", "bind"),
        ("libc", "::", "syscall"),
        ("libc", "::", "dlopen"),
        ("libc", "::", "dlsym"),
        ("TcpStream", "::", "connect"),
        ("TcpListener", "::", "bind"),
        ("UdpSocket", "::", "bind"),
    ]
    for sequence in direct_sequences:
        index = find_sequence(values, sequence)
        if index is not None:
            return index

    for index, value in enumerate(values):
        if value != "use":
            continue
        end = index + 1
        depth = 0
        while end < len(values):
            if values[end] in {"{", "(", "["}:
                depth += 1
            elif values[end] in {"}", ")", "]"}:
                depth -= 1
            elif values[end] == ";" and depth == 0:
                break
            end += 1
        use_tree = values[index + 1 : end]
        if "include" in use_tree:
            return index
        if "libc" in use_tree:
            libc_index = use_tree.index("libc")
            aliases_libc = (
                "*" in use_tree
                or any(symbol in use_tree for symbol in SOCKET_FUNCTIONS)
                or sequence_at(use_tree, libc_index, ("libc", "as"))
                or find_sequence(use_tree, ("self", "as")) is not None
            )
            if aliases_libc:
                return index
        if "std" in use_tree and "net" in use_tree:
            net_index = use_tree.index("net")
            imports_net_module = (
                net_index + 1 == len(use_tree)
                or use_tree[net_index + 1] != "::"
            )
            if (
                imports_net_module
                or "*" in use_tree
                or any(socket_type in use_tree for socket_type in SOCKET_TYPES)
            ):
                return index

    for index in range(len(values) - 4):
        if values[index : index + 3] == ["extern", "crate", "libc"] and "as" in values[index + 3 : index + 5]:
            return index

    for index, value in enumerate(values):
        if value != "extern":
            continue
        try:
            start = values.index("{", index + 1)
        except ValueError:
            continue
        depth = 1
        end = start + 1
        while end < len(values) and depth:
            if values[end] == "{":
                depth += 1
            elif values[end] == "}":
                depth -= 1
            end += 1
        if any(symbol in values[start + 1 : end - 1] for symbol in SOCKET_FUNCTIONS - {"syscall"}):
            return index
    return None


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

    workspace_names = {packages_by_id[package_id]["name"] for package_id in workspace_ids}
    if workspace_names != EXPECTED_WORKSPACE:
        blocked.add(f"unreviewed workspace package set: {sorted(workspace_names)}")

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
            canonical_edge_name = dependency_name.replace("-", "_")
            if dependency["name"] != canonical_edge_name:
                approved = False
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
            if any(kind in {"custom-build", "proc-macro"} for kind in target["kind"]):
                blocked.add(
                    f"workspace generated-code target is not allowed: {target['src_path']}"
                )
            target_path = Path(target["src_path"])
            if target_path.is_symlink() or not target_path.is_file():
                blocked.add(f"Cargo target source is not a readable file: {target_path}")
            try:
                resolved_target = target_path.resolve(strict=True)
                resolved_package = package_dir.resolve(strict=True)
                resolved_target.relative_to(resolved_package)
            except (OSError, ValueError):
                blocked.add(f"Cargo target source is outside its package: {target_path}")
        for root in roots:
            collect_sources(root)

    for source_file in sorted(source_files):
        try:
            source = source_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            blocked.add(f"could not read Rust source {source_file}: {error}")
            continue
        try:
            code = strip_comments_and_literals(source)
        except ValueError as error:
            blocked.add(f"could not parse Rust source {source_file}: {error}")
            continue
        tokens = rust_tokens(code)
        violation = source_violation(tokens)
        if violation is not None:
            offset = tokens[violation][1]
            line = code.count("\n", 0, offset) + 1
            blocked.add(f"direct socket API: {source_file}:{line}")
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
