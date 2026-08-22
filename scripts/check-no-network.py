#!/usr/bin/env python3
"""Validate Phantom's locked dependency graph and Rust socket authorities."""

import hashlib
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
    "phantom-emu": {
        "alacritty_terminal": "0.26.0", "regex-automata": "0.4.15",
        "regex-syntax": "0.8.11",
    },
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
    "regex-automata": {
        "alloc", "default", "dfa", "dfa-build", "dfa-onepass", "dfa-search",
        "hybrid", "meta", "nfa", "nfa-backtrack", "nfa-pikevm", "nfa-thompson",
        "perf", "perf-inline", "perf-literal", "perf-literal-multisubstring",
        "perf-literal-substring", "std", "syntax", "unicode", "unicode-age",
        "unicode-bool", "unicode-case", "unicode-gencat", "unicode-perl",
        "unicode-script", "unicode-segment", "unicode-word-boundary",
    },
    "rusqlite": {
        "blob", "bundled", "cache", "default", "ffi-sqlite-wasm-rs", "hashlink",
        "modern_sqlite",
    },
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

TOKEN_RE = re.compile(r"r#[^\W\d]\w*|[^\W\d]\w*|::|->|=>|[^\s]")
IDENTIFIER_RE = re.compile(r"[^\W\d]\w*")
SOCKET_TYPES = {"TcpStream", "TcpListener", "UdpSocket", "ToSocketAddrs"}
SOCKET_FUNCTIONS = {
    "accept", "accept4", "bind", "connect", "dlopen", "dlsym", "getaddrinfo",
    "getaddrinfo_a", "gethostbyaddr", "gethostbyname", "gethostbyname2",
    "getnameinfo", "listen", "recv", "recvfrom", "res_query", "res_search",
    "res_send", "send", "sendto", "socket", "socketpair", "syscall",
}
BUILTIN_MACROS = {
    "assert", "assert_eq", "assert_ne", "cfg", "env", "eprintln", "format",
    "include_bytes", "include_str", "json", "matches", "panic", "params", "print",
    "println", "vec", "vertex_attr_array", "write",
}

APPROVED_CUSTOM_MACRO_FILES = {
    "crates/phantom-emu/src/alacritty_core.rs": (
        "af4782addaacb9a2df8b43660e810d81d30ca4b0e45203d6ac4ffe1396f9987b",
        {"reset_state"},
    ),
    "crates/phantom-gfx/tests/headless.rs": (
        "194bf25be61f92c20873cf26e90660e1269cbf42efa2f167048d8d05b6229b74",
        {"harness_or_skip"},
    ),
}

APPROVED_REPO_PACKAGES = {
    ("wayland-scanner", "0.31.10"): (
        "vendor/wayland-scanner/Cargo.toml",
        "0198a708d4093f85eebfe9c6356402b4670c42e4dc16d1df1f8773c77fbeb1b2",
        {
            "bitflags", "debug_assert", "failed", "format_ident",
            "generate_client_code", "generate_interfaces", "print", "quote",
            "reference", "unimplemented", "unreachable",
        },
    ),
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

        quote = i + 1 if source.startswith("b'", i) else i
        if quote < length and source[quote] == "'":
            content = quote + 1
            end_content = content
            if content < length and source[content] == "\\":
                end_content = content + 2
                if source.startswith(("\\u{", "\\U{"), content):
                    brace = source.find("}", content + 3)
                    if brace < 0:
                        raise ValueError(f"unterminated character literal at byte {i}")
                    end_content = brace + 1
                elif source.startswith("\\x", content):
                    end_content = content + 4
            elif content < length and source[content] not in "'\n":
                end_content = content + 1

            if end_content < length and source[end_content] == "'":
                end = end_content + 1
                for offset in range(i, end):
                    result[offset] = " "
                i = end
                continue

            # An apostrophe followed by an identifier is a lifetime or label.
            # A lifetime in expression-assignment position followed by `;` is
            # instead an unterminated character literal and must fail closed.
            if quote == i:
                lifetime = re.match(r"'(?:[^\W\d]\w*|_)", source[i:])
                if lifetime:
                    end = i + lifetime.end()
                    previous = source[:i].rstrip()
                    following = source[end:].lstrip()
                    if previous.endswith("=") and following.startswith(";"):
                        raise ValueError(f"unterminated character literal at byte {i}")
                    i = end
                    continue
            raise ValueError(f"unterminated character literal at byte {i}")
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


def source_violation(tokens, approved_custom_macros):
    values = [value for value, _ in tokens]

    delimiters = {"(": ")", "[": "]", "{": "}"}
    stack = []
    matching_delimiters = {}
    for index, value in enumerate(values):
        if value in delimiters:
            stack.append((delimiters[value], index))
        elif value in delimiters.values():
            if not stack or stack[-1][0] != value:
                return index
            _, opening_index = stack.pop()
            matching_delimiters[opening_index] = index
    if stack:
        return stack[-1][1]

    for index in range(len(values) - 2):
        if values[index : index + 2] != ["macro_rules", "!"]:
            continue
        if values[index + 2] not in approved_custom_macros:
            return index

    for index in range(len(values) - 2):
        if values[index + 1] != "!" or values[index + 2] not in {"(", "[", "{"}:
            continue
        macro_name = values[index]
        if not IDENTIFIER_RE.fullmatch(macro_name) or macro_name in {"if", "while"}:
            continue
        if macro_name not in BUILTIN_MACROS and macro_name not in approved_custom_macros:
            return index

    for index in range(len(values) - 1):
        if values[index : index + 2] != ["#", "["]:
            continue
        end = matching_delimiters[index + 1]
        attribute = values[index + 2 : end]
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
        ("std", "::", "net", "::", "ToSocketAddrs"),
        ("net", "::", "TcpStream"),
        ("net", "::", "TcpListener"),
        ("net", "::", "UdpSocket"),
        ("net", "::", "ToSocketAddrs"),
        ("tokio", "::", "net"),
        ("async_std", "::", "net"),
        ("smol", "::", "net"),
        ("mio", "::", "net"),
        ("socket2", "::"),
        ("nix", "::", "sys", "::", "socket"),
        ("TcpStream", "::", "connect"),
        ("TcpListener", "::", "bind"),
        ("UdpSocket", "::", "bind"),
    ]
    direct_sequences.extend(
        ("libc", "::", function) for function in sorted(SOCKET_FUNCTIONS)
    )
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
        if any(
            use_tree[position] == "as" and use_tree[position + 1] in BUILTIN_MACROS
            for position in range(len(use_tree) - 1)
        ):
            return index
        for root in {"std", "core"}:
            root_aliases = [
                (root, "as"),
                (root, "::", "*"),
                (root, "::", "{", "self", "as"),
            ]
            if any(find_sequence(use_tree, alias) is not None for alias in root_aliases):
                return index
            root_index = use_tree.index(root) if root in use_tree else None
            if root_index is not None and (
                root_index + 1 == len(use_tree)
                or use_tree[root_index + 1] in {",", "}"}
            ):
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
                or "self" in use_tree
                or any(socket_type in use_tree for socket_type in SOCKET_TYPES)
            ):
                return index

    for index in range(len(values) - 4):
        if (
            values[index : index + 2] == ["extern", "crate"]
            and values[index + 2] in {"core", "libc", "std"}
            and "as" in values[index + 3 : index + 5]
        ):
            return index

    for index, value in enumerate(values):
        if value != "extern":
            continue
        if index + 1 >= len(values) or values[index + 1] != "{":
            continue
        start = index + 1
        depth = 1
        end = start + 1
        while end < len(values) and depth:
            if values[end] == "{":
                depth += 1
            elif values[end] == "}":
                depth -= 1
            end += 1
        if any(symbol in values[start + 1 : end - 1] for symbol in SOCKET_FUNCTIONS):
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


def tree_digest(root):
    files = []

    def raise_walk_error(error):
        raise error

    for current, directories, filenames in os.walk(
        root, onerror=raise_walk_error, followlinks=False
    ):
        current_path = Path(current)
        for directory in directories:
            if (current_path / directory).is_symlink():
                raise OSError(f"symlinked directory in reviewed package: {directory}")
        for filename in filenames:
            file_path = current_path / filename
            if file_path.is_symlink():
                raise OSError(f"symlinked file in reviewed package: {file_path}")
            files.append(file_path)

    digest = hashlib.sha256()
    for file_path in sorted(files):
        digest.update(str(file_path.relative_to(root)).encode("utf-8"))
        digest.update(b"\0")
        digest.update(file_path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def validate_graph(metadata):
    packages_by_id = {package["id"]: package for package in metadata["packages"]}
    nodes_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace_ids = set(metadata["workspace_members"])
    blocked = set()

    workspace_names = {packages_by_id[package_id]["name"] for package_id in workspace_ids}
    if workspace_names != EXPECTED_WORKSPACE:
        blocked.add(f"unreviewed workspace package set: {sorted(workspace_names)}")

    resolved_names = {packages_by_id[package_id]["name"] for package_id in nodes_by_id}
    for package_id in nodes_by_id:
        if package_id in workspace_ids:
            continue
        package = packages_by_id[package_id]
        if package["source"] not in {None, CRATES_IO}:
            blocked.add(
                f"unreviewed package source: {package['name']} {package['version']} "
                f"({package['source']})"
            )
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
    repo_macro_roots = []
    workspace_root = Path(metadata["workspace_root"]).resolve()

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

    workspace_ids = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] in workspace_ids or package["source"] is not None:
            continue
        manifest_path = Path(package["manifest_path"])
        try:
            resolved_manifest = manifest_path.resolve(strict=True)
        except OSError as error:
            blocked.add(
                f"could not resolve repository package {package['name']} "
                f"{package['version']}: {error}"
            )
            continue
        try:
            relative_manifest = str(resolved_manifest.relative_to(workspace_root))
        except ValueError:
            blocked.add(
                f"repository package is outside workspace: {package['name']} "
                f"{package['version']} at {resolved_manifest}"
            )
            continue
        approved = APPROVED_REPO_PACKAGES.get((package["name"], package["version"]))
        if approved is None or relative_manifest != approved[0]:
            blocked.add(
                f"unreviewed repository package: {package['name']} {package['version']} "
                f"at {relative_manifest}"
            )
            continue
        package_root = resolved_manifest.parent
        try:
            actual_digest = tree_digest(package_root)
        except OSError as error:
            blocked.add(f"could not hash repository package {package_root}: {error}")
            continue
        if actual_digest != approved[1]:
            blocked.add(f"repository package source changed: {relative_manifest}")
        for target in package["targets"]:
            if "custom-build" in target["kind"]:
                blocked.add(
                    f"repository generated-code target is not allowed: {target['src_path']}"
                )
            target_path = Path(target["src_path"])
            try:
                resolved_target = target_path.resolve(strict=True)
                resolved_target.relative_to(package_root)
            except (OSError, ValueError):
                blocked.add(
                    f"repository target source is outside its package: {target_path}"
                )
            if target_path.is_symlink() or not target_path.is_file():
                blocked.add(f"repository target source is not a readable file: {target_path}")
        repo_macro_roots.append((package_root, approved[2]))
        collect_sources(package_root)

    for source_file in sorted(source_files):
        try:
            source = source_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            blocked.add(f"could not read Rust source {source_file}: {error}")
            continue
        approved_custom_macros = set()
        try:
            relative_source = str(source_file.resolve(strict=True).relative_to(workspace_root))
        except (OSError, ValueError):
            relative_source = None
        if relative_source in APPROVED_CUSTOM_MACRO_FILES:
            expected_digest, macro_names = APPROVED_CUSTOM_MACRO_FILES[relative_source]
            if hashlib.sha256(source.encode("utf-8")).hexdigest() != expected_digest:
                blocked.add(f"reviewed custom macro source changed: {source_file}")
            else:
                approved_custom_macros.update(macro_names)
        for repo_root, macro_names in repo_macro_roots:
            try:
                source_file.resolve(strict=True).relative_to(repo_root)
            except (OSError, ValueError):
                continue
            approved_custom_macros.update(macro_names)

        try:
            code = strip_comments_and_literals(source)
        except ValueError as error:
            blocked.add(f"could not parse Rust source {source_file}: {error}")
            continue
        tokens = rust_tokens(code)
        violation = source_violation(tokens, approved_custom_macros)
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
