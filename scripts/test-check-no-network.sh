#!/usr/bin/env bash
# Fixture tests for scripts/check-no-network.sh.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
gate="$script_dir/check-no-network.sh"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/phantom-no-network-tests.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

new_fixture() {
  fixture=$1
  package_name=${2:-phantom-core}
  mkdir -p "$fixture/crates/app/src"
  members='"crates/app"'
  for workspace_name in phantom-app phantom-core phantom-emu phantom-gfx; do
    if [ "$workspace_name" = "$package_name" ]; then
      continue
    fi
    stub="$fixture/crates/stub-$workspace_name"
    mkdir -p "$stub/src"
    members="$members, \"crates/stub-$workspace_name\""
    printf '%s\n' \
      '[package]' \
      "name = \"$workspace_name\"" \
      'version = "0.1.0"' \
      'edition = "2021"' >"$stub/Cargo.toml"
    printf '%s\n' 'pub fn local_only() {}' >"$stub/src/lib.rs"
  done
  printf '%s\n' \
    '[workspace]' \
    "members = [$members]" \
    'resolver = "2"' >"$fixture/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    "name = \"$package_name\"" \
    'version = "0.1.0"' \
    'edition = "2021"' >"$fixture/crates/app/Cargo.toml"
  printf '%s\n' 'pub fn local_only() {}' >"$fixture/crates/app/src/lib.rs"
}

new_dependency() {
  dependency=$1
  package_name=$2
  version=${3:-99.0.0}
  mkdir -p "$dependency/src"
  printf '%s\n' \
    '[package]' \
    "name = \"$package_name\"" \
    "version = \"$version\"" \
    'edition = "2021"' >"$dependency/Cargo.toml"
  printf '%s\n' 'pub fn dependency() {}' >"$dependency/src/lib.rs"
}

run_gate() {
  PHANTOM_NO_NETWORK_ROOT=$1 "$gate"
}

expect_failure() {
  fixture=$1
  expected=$2
  if output=$(run_gate "$fixture" 2>&1); then
    echo "fixture unexpectedly passed: $fixture" >&2
    exit 1
  fi
  if ! grep -q "$expected" <<<"$output"; then
    echo "$output" >&2
    echo "fixture did not report expected finding: $expected" >&2
    exit 1
  fi
}

echo "==> repository's reviewed AccessKit graph passes"
"$gate"

echo "==> unreviewed egui-winit link features fail"
feature_metadata="$scratch/feature-metadata.json"
cargo metadata --locked --format-version 1 >"$feature_metadata"
python3 - "$feature_metadata" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as metadata_file:
    metadata = json.load(metadata_file)
packages = {package["id"]: package for package in metadata["packages"]}
for node in metadata["resolve"]["nodes"]:
    if packages[node["id"]]["name"] == "egui-winit":
        node["features"].append("links")
    if packages[node["id"]]["name"] == "phantom-core":
        for dependency in node["deps"]:
            if packages[dependency["pkg"]]["name"] == "libc":
                dependency["name"] = "native"
with open(path, "w", encoding="utf-8") as metadata_file:
    json.dump(metadata, metadata_file)
PY
if output=$(python3 "$script_dir/check-no-network.py" "$feature_metadata" 2>&1); then
  echo "unreviewed egui-winit features unexpectedly passed" >&2
  exit 1
fi
grep -q 'unreviewed features for egui-winit' <<<"$output"
grep -q 'unreviewed direct dependency identity: phantom-core -> libc' <<<"$output"

echo "==> harmless address types, comments, and strings pass"
clean="$scratch/clean"
new_fixture "$clean"
printf '%s\n' \
  'pub const EXAMPLE: &str = "std::net::TcpStream::connect";' \
  'pub const ATTRIBUTE_EXAMPLE: &str = r##"#[link_name = "socket"]"##;' \
  'pub fn link_name() {}' \
  '// std::net::UdpSocket::bind is forbidden in executable code.' \
  'use libc::{openat, O_CLOEXEC};' \
  'pub fn local_address(port: u16) -> std::net::SocketAddr {' \
  '    let _ = (openat, O_CLOEXEC);' \
  '    std::net::SocketAddr::from(([127, 0, 0, 1], port))' \
  '}' >"$clean/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$clean/Cargo.toml"
run_gate "$clean"

echo "==> workspace source outside crates is scanned"
outside="$scratch/outside-crates"
mkdir -p "$outside/components/core/src"
printf '%s\n' \
  '[workspace]' \
  'members = ["components/core"]' \
  'resolver = "2"' >"$outside/Cargo.toml"
printf '%s\n' \
  '[package]' \
  'name = "phantom-core"' \
  'version = "0.1.0"' \
  'edition = "2021"' >"$outside/components/core/Cargo.toml"
printf '%s\n' 'pub fn bind() { let _ = std::net::UdpSocket::bind("127.0.0.1:0"); }' \
  >"$outside/components/core/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$outside/Cargo.toml"
expect_failure "$outside" 'direct socket API:'

echo "==> harmless mixed grouped imports pass and compile"
mixed_imports="$scratch/mixed-imports"
new_fixture "$mixed_imports"
printf '%s\n' \
  'use std::{fs::{self, File}, net::SocketAddr};' \
  'pub fn local_only() {' \
  '    let _ = fs::metadata(".");' \
  '    let _ = core::mem::size_of::<(File, SocketAddr)>();' \
  '}' >"$mixed_imports/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$mixed_imports/Cargo.toml"
cargo check --offline --manifest-path "$mixed_imports/Cargo.toml" >/dev/null 2>&1
run_gate "$mixed_imports"

for client in reqwest minreq; do
  echo "==> renamed $client dependency fails"
  renamed="$scratch/renamed-$client"
  dependency="$scratch/dependencies/$client-shim"
  new_fixture "$renamed"
  new_dependency "$dependency" "$client"
  printf '%s\n' \
    '[package]' \
    'name = "phantom-core"' \
    'version = "0.1.0"' \
    'edition = "2021"' \
    '' \
    '[dependencies]' \
    "transport = { package = \"$client\", path = \"../../../dependencies/$client-shim\" }" \
    >"$renamed/crates/app/Cargo.toml"
  cargo generate-lockfile --offline --manifest-path "$renamed/Cargo.toml"
  expect_failure "$renamed" "resolved network client: $client"
done

echo "==> unreviewed direct dependency fails even with an unknown name"
unknown="$scratch/unknown-client"
unknown_dependency="$scratch/dependencies/innocent-wrapper"
new_fixture "$unknown"
new_dependency "$unknown_dependency" "innocent-wrapper"
printf '%s\n' \
  '[package]' \
  'name = "phantom-core"' \
  'version = "0.1.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  'helper = { package = "innocent-wrapper", path = "../../../dependencies/innocent-wrapper" }' \
  >"$unknown/crates/app/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$unknown/Cargo.toml"
cargo check --offline --manifest-path "$unknown/Cargo.toml" >/dev/null 2>&1
expect_failure "$unknown" 'unreviewed direct dependency identity: phantom-core -> innocent-wrapper'
unknown_output=$(run_gate "$unknown" 2>&1 || true)
grep -q 'repository package is outside workspace: innocent-wrapper' <<<"$unknown_output"

echo "==> non-crates.io package sources fail even with an approved identity"
git_source="$scratch/git-source"
git_dependency="$scratch/dependencies/directories-git"
new_fixture "$git_source"
new_dependency "$git_dependency" directories 6.0.0
git -C "$git_dependency" init -q
git -C "$git_dependency" add Cargo.toml src/lib.rs
git -C "$git_dependency" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -qm fixture
printf '%s\n' \
  '[package]' \
  'name = "phantom-core"' \
  'version = "0.1.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  "directories = { git = \"file://$git_dependency\" }" \
  >"$git_source/crates/app/Cargo.toml"
# This fetches only the fixture's local file:// repository into Cargo's cache.
cargo generate-lockfile --manifest-path "$git_source/Cargo.toml"
cargo check --offline --manifest-path "$git_source/Cargo.toml" >/dev/null 2>&1
expect_failure "$git_source" 'unreviewed package source: directories 6.0.0'

echo "==> unauthorized transitive socket transport fails"
transitive="$scratch/transitive"
wrapper="$scratch/dependencies/directories-wrapper"
socket2="$scratch/dependencies/socket2-shim"
new_fixture "$transitive"
new_dependency "$wrapper" "directories"
new_dependency "$socket2" "socket2"
printf '%s\n' \
  '[package]' \
  'name = "directories"' \
  'version = "99.0.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  'transport = { package = "socket2", path = "../socket2-shim" }' >"$wrapper/Cargo.toml"
printf '%s\n' \
  '[package]' \
  'name = "phantom-core"' \
  'version = "0.1.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  'directories = { path = "../../../dependencies/directories-wrapper" }' \
  >"$transitive/crates/app/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$transitive/Cargo.toml"
expect_failure "$transitive" 'resolved socket transport: socket2'

echo "==> name-spoofed AccessKit ancestry fails identity checks"
accesskit="$scratch/accesskit"
new_fixture "$accesskit" phantom-app
for package in egui-winit accesskit_winit accesskit_unix; do
  new_dependency "$scratch/dependencies/$package" "$package" 99.0.0
done
new_dependency "$scratch/dependencies/zbus" zbus 5.19.0
new_dependency "$scratch/dependencies/async-io" async-io 2.6.0
printf '%s\n' '' '[dependencies]' \
  'egui-winit = { path = "../../../dependencies/egui-winit" }' \
  >>"$accesskit/crates/app/Cargo.toml"
printf '%s\n' '' '[dependencies]' \
  'accesskit_winit = { path = "../accesskit_winit" }' \
  >>"$scratch/dependencies/egui-winit/Cargo.toml"
printf '%s\n' '' '[dependencies]' \
  'accesskit_unix = { path = "../accesskit_unix" }' \
  >>"$scratch/dependencies/accesskit_winit/Cargo.toml"
printf '%s\n' '' '[dependencies]' \
  'zbus = { path = "../zbus" }' \
  >>"$scratch/dependencies/accesskit_unix/Cargo.toml"
printf '%s\n' '' '[dependencies]' \
  'async-io = { path = "../async-io" }' \
  >>"$scratch/dependencies/zbus/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$accesskit/Cargo.toml"
expect_failure "$accesskit" 'unreviewed direct dependency identity:'

echo "==> unauthorized zbus ancestry fails"
unauthorized="$scratch/unauthorized-zbus"
new_fixture "$unauthorized"
printf '%s\n' '' '[dependencies]' \
  'directories = { package = "zbus", path = "../../../dependencies/zbus" }' \
  >>"$unauthorized/crates/app/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$unauthorized/Cargo.toml"
expect_failure "$unauthorized" 'unauthorized path to zbus'

echo "==> direct std::net use fails"
socket="$scratch/socket"
new_fixture "$socket"
printf '%s\n' \
  'pub fn connect() {' \
  '    let _ = std::net::TcpStream::connect("127.0.0.1:9");' \
  '}' >"$socket/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$socket/Cargo.toml"
expect_failure "$socket" 'direct socket API:'

echo "==> raw libc FFI socket declarations fail"
raw_ffi="$scratch/raw-ffi"
new_fixture "$raw_ffi"
printf '%s\n' \
  'unsafe extern "C" {' \
  '    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;' \
  '}' \
  'pub fn raw() { let _ = socket; }' \
  >"$raw_ffi/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$raw_ffi/Cargo.toml"
expect_failure "$raw_ffi" 'direct socket API:'

echo "==> raw extern syscall declarations fail and compile"
extern_syscall="$scratch/extern-syscall"
new_fixture "$extern_syscall"
printf '%s\n' \
  'unsafe extern "C" {' \
  '    fn syscall(number: i64) -> i64;' \
  '}' \
  'pub fn raw() { let _ = syscall; }' \
  >"$extern_syscall/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$extern_syscall/Cargo.toml"
cargo check --offline --manifest-path "$extern_syscall/Cargo.toml" >/dev/null 2>&1
expect_failure "$extern_syscall" 'direct socket API:'

echo "==> ToSocketAddrs imports fail and compile"
socket_addresses="$scratch/socket-addresses"
new_fixture "$socket_addresses"
printf '%s\n' \
  'use std::net::ToSocketAddrs;' \
  'pub fn resolve() { let _ = ("localhost", 9).to_socket_addrs(); }' \
  >"$socket_addresses/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$socket_addresses/Cargo.toml"
cargo check --offline --manifest-path "$socket_addresses/Cargo.toml" >/dev/null 2>&1
expect_failure "$socket_addresses" 'direct socket API:'

echo "==> whole std and core authority aliases fail and compile"
root_alias="$scratch/root-alias"
new_fixture "$root_alias"
printf '%s\n' \
  'use std as system;' \
  'use core as language_core;' \
  'pub fn connect() {' \
  '    let _ = system::net::TcpStream::connect("127.0.0.1:9");' \
  '    let _ = language_core::mem::size_of::<usize>();' \
  '}' >"$root_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$root_alias/Cargo.toml"
cargo check --offline --manifest-path "$root_alias/Cargo.toml" >/dev/null 2>&1
expect_failure "$root_alias" 'direct socket API:'

echo "==> extern-crate std authority aliases fail and compile"
extern_root_alias="$scratch/extern-root-alias"
new_fixture "$extern_root_alias"
printf '%s\n' \
  'extern crate std as system;' \
  'use system::net as transport;' \
  'use transport::TcpStream as Stream;' \
  'pub fn dial() {' \
  '    let _ = Stream::connect("127.0.0.1:9");' \
  '}' >"$extern_root_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$extern_root_alias/Cargo.toml"
cargo check --offline --manifest-path "$extern_root_alias/Cargo.toml" >/dev/null 2>&1
expect_failure "$extern_root_alias" 'direct socket API:'

echo "==> nested std::net module aliases fail and compile"
nested_net_alias="$scratch/nested-net-alias"
new_fixture "$nested_net_alias"
printf '%s\n' \
  'use std::net::{self as transport};' \
  'use transport::TcpStream as Stream;' \
  'pub fn connect() {' \
  '    let _ = Stream::connect("127.0.0.1:9");' \
  '}' >"$nested_net_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$nested_net_alias/Cargo.toml"
cargo check --offline --manifest-path "$nested_net_alias/Cargo.toml" >/dev/null 2>&1
expect_failure "$nested_net_alias" 'direct socket API:'

echo "==> grouped std::net self imports cannot seed alias chains"
grouped_net_self="$scratch/grouped-net-self"
new_fixture "$grouped_net_self"
printf '%s\n' \
  'use std::net::{self};' \
  'use net as transport;' \
  'use transport::TcpStream as Stream;' \
  'pub fn connect() {' \
  '    let _ = Stream::connect("127.0.0.1:9");' \
  '}' >"$grouped_net_self/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$grouped_net_self/Cargo.toml"
cargo check --offline --manifest-path "$grouped_net_self/Cargo.toml" >/dev/null 2>&1
expect_failure "$grouped_net_self" 'direct socket API:'

echo "==> ambient net tokens cannot hide a later std::net authority"
mixed_net_authority="$scratch/mixed-net-authority"
new_fixture "$mixed_net_authority"
printf '%s\n' \
  'mod foo { pub mod net { pub struct Thing; } }' \
  'use {foo::net::Thing, std::net as transport};' \
  'use transport::TcpStream as Stream;' \
  'pub fn dial() {' \
  '    let _ = core::mem::size_of::<Thing>();' \
  '    let _ = Stream::connect("127.0.0.1:9");' \
  '}' >"$mixed_net_authority/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$mixed_net_authority/Cargo.toml"
cargo check --offline --manifest-path "$mixed_net_authority/Cargo.toml" >/dev/null 2>&1
expect_failure "$mixed_net_authority" 'direct socket API:'

echo "==> libc module aliases fail"
libc_module_alias="$scratch/libc-module-alias"
new_fixture "$libc_module_alias"
printf '%s\n' \
  'extern crate libc as c;' \
  'use ::libc as native;' \
  'pub fn aliases() { let _ = (c::socket, native::connect); }' \
  >"$libc_module_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$libc_module_alias/Cargo.toml"
expect_failure "$libc_module_alias" 'direct socket API:'

echo "==> grouped and qualified libc module aliases fail"
qualified_libc_alias="$scratch/qualified-libc-alias"
new_fixture "$qualified_libc_alias"
printf '%s\n' \
  'use {libc as c};' \
  'use self::libc as native;' \
  'pub fn aliases() { let _ = (c::socket, native::connect); }' \
  >"$qualified_libc_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$qualified_libc_alias/Cargo.toml"
expect_failure "$qualified_libc_alias" 'direct socket API:'

echo "==> libc glob imports and local re-exports fail"
libc_glob="$scratch/libc-glob"
new_fixture "$libc_glob"
printf '%s\n' \
  'mod native { pub use libc::*; }' \
  'use native::*;' \
  'pub fn raw() { let _ = socket; }' \
  >"$libc_glob/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$libc_glob/Cargo.toml"
expect_failure "$libc_glob" 'direct socket API:'

echo "==> raw FFI link-name aliases fail"
link_name="$scratch/link-name"
new_fixture "$link_name"
printf '%s\n' \
  'unsafe extern "C" {' \
  '    #[link_name = "socket"]' \
  '    fn open_socket(domain: i32, kind: i32, protocol: i32) -> i32;' \
  '}' \
  >"$link_name/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$link_name/Cargo.toml"
expect_failure "$link_name" 'direct socket API:'

echo "==> every link-name spelling fails without literal false positives"
link_name_variants="$scratch/link-name-variants"
new_fixture "$link_name_variants"
printf '%s\n' \
  'unsafe extern "C" {' \
  '    #[link_name = r#"socket"#]' \
  '    fn raw_name();' \
  '    #[link_name = "sock\x65t"]' \
  '    fn escaped_name();' \
  '    #[link_name = concat!("sock", "et")]' \
  '    fn concatenated_name();' \
  '    #[cfg_attr(unix, link_name = "socket")]' \
  '    fn conditional_name();' \
  '}' \
  >"$link_name_variants/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$link_name_variants/Cargo.toml"
expect_failure "$link_name_variants" 'direct socket API:'

echo "==> out-of-root compiler inputs fail closed"
compiler_input="$scratch/compiler-input"
new_fixture "$compiler_input"
printf '%s\n' \
  '#[path = "../../../network.rs"]' \
  'mod network;' \
  'include!("../../../more_network.rs");' \
  >"$compiler_input/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$compiler_input/Cargo.toml"
expect_failure "$compiler_input" 'direct socket API:'

echo "==> conditional path and aliased include compiler inputs fail and compile"
for compiler_escape in cfg-path include-alias; do
  escape_fixture="$scratch/$compiler_escape"
  new_fixture "$escape_fixture"
  if [ "$compiler_escape" = cfg-path ]; then
    printf '%s\n' \
      '#[cfg_attr(unix, path = "../../../network.rs")]' \
      'mod network;' >"$escape_fixture/crates/app/src/lib.rs"
    printf '%s\n' \
      'pub fn socket() { let _ = std::net::TcpStream::connect("127.0.0.1:9"); }' \
      >"$escape_fixture/network.rs"
  else
    printf '%s\n' \
      'use std::include as bring;' \
      'bring!("../../../included.rs");' >"$escape_fixture/crates/app/src/lib.rs"
    printf '%s\n' \
      'pub fn socket() { let _ = std::net::TcpStream::connect("127.0.0.1:9"); }' \
      >"$escape_fixture/included.rs"
  fi
  cargo generate-lockfile --offline --manifest-path "$escape_fixture/Cargo.toml"
  cargo check --offline --manifest-path "$escape_fixture/Cargo.toml" >/dev/null 2>&1
  expect_failure "$escape_fixture" 'direct socket API:'
done

echo "==> renamed approved libc authority fails and compiles"
renamed_libc="$scratch/renamed-libc"
compile_libc="$scratch/dependencies/compile-libc"
new_fixture "$renamed_libc"
new_dependency "$compile_libc" libc 0.2.186
printf '%s\n' \
  'pub unsafe fn socket(_: i32, _: i32, _: i32) -> i32 { -1 }' \
  'pub unsafe fn getaddrinfo(_: usize, _: usize, _: usize, _: usize) -> i32 { -1 }' \
  'pub unsafe fn dlsym(_: usize, _: usize) -> usize { 0 }' \
  'pub const SYS_socket: i64 = 1;' \
  'pub unsafe fn syscall(_: i64, _: i32, _: i32, _: i32) -> i64 { -1 }' \
  >"$compile_libc/src/lib.rs"
printf '%s\n' '' '[dependencies]' \
  'native = { package = "libc", path = "../../../dependencies/compile-libc" }' \
  >>"$renamed_libc/crates/app/Cargo.toml"
printf '%s\n' \
  'use native::*;' \
  'pub fn open() { let _ = unsafe { socket(0, 0, 0) }; }' \
  >"$renamed_libc/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$renamed_libc/Cargo.toml"
cargo check --offline --manifest-path "$renamed_libc/Cargo.toml" >/dev/null 2>&1
expect_failure "$renamed_libc" 'unreviewed direct dependency identity:'

echo "==> ambient libc tokens cannot hide a later libc authority"
mixed_libc_authority="$scratch/mixed-libc-authority"
new_fixture "$mixed_libc_authority"
printf '%s\n' '' '[dependencies]' \
  'libc = { path = "../../../dependencies/compile-libc" }' \
  >>"$mixed_libc_authority/crates/app/Cargo.toml"
printf '%s\n' \
  'extern crate libc;' \
  'mod foo { pub mod libc { pub struct Thing; } }' \
  'use {foo::libc::Thing, libc as native};' \
  'use native::socket as open_socket;' \
  'pub fn raw() {' \
  '    let _ = core::mem::size_of::<Thing>();' \
  '    let _ = open_socket;' \
  '}' >"$mixed_libc_authority/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$mixed_libc_authority/Cargo.toml"
cargo check --offline --manifest-path "$mixed_libc_authority/Cargo.toml" >/dev/null 2>&1
expect_failure "$mixed_libc_authority" 'direct socket API:'

echo "==> libc syscall socket authority fails and compiles"
syscall_fixture="$scratch/libc-syscall"
new_fixture "$syscall_fixture"
printf '%s\n' '' '[dependencies]' \
  'libc = { path = "../../../dependencies/compile-libc" }' \
  >>"$syscall_fixture/crates/app/Cargo.toml"
printf '%s\n' \
  'pub fn open() {' \
  '    let _ = unsafe { libc::syscall(libc::SYS_socket, 0, 0, 0) };' \
  '    let _ = unsafe { libc::dlsym(0, 0) };' \
  '}' >"$syscall_fixture/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$syscall_fixture/Cargo.toml"
cargo check --offline --manifest-path "$syscall_fixture/Cargo.toml" >/dev/null 2>&1
expect_failure "$syscall_fixture" 'direct socket API:'

echo "==> raw and libc DNS resolution authorities fail and compile"
dns_fixture="$scratch/dns-resolution"
new_fixture "$dns_fixture"
printf '%s\n' '' '[dependencies]' \
  'libc = { path = "../../../dependencies/compile-libc" }' \
  >>"$dns_fixture/crates/app/Cargo.toml"
printf '%s\n' \
  'unsafe extern "C" {' \
  '    fn getaddrinfo(node: usize, service: usize, hints: usize, result: usize) -> i32;' \
  '}' \
  'pub fn resolve() {' \
  '    let _ = getaddrinfo;' \
  '    let _ = libc::getaddrinfo;' \
  '}' >"$dns_fixture/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$dns_fixture/Cargo.toml"
cargo check --offline --manifest-path "$dns_fixture/Cargo.toml" >/dev/null 2>&1
expect_failure "$dns_fixture" 'direct socket API:'

echo "==> workspace build scripts fail before generated Rust can compile"
build_script="$scratch/build-script"
new_fixture "$build_script"
printf '%s\n' 'mod generated;' >"$build_script/crates/app/src/lib.rs"
printf '%s\n' \
  'fn main() {' \
  '    std::fs::write(' \
  '        "src/generated.rs",' \
  '        "pub fn socket() { let _ = std::net::TcpStream::connect(\"127.0.0.1:9\"); }",' \
  '    ).unwrap();' \
  '}' >"$build_script/crates/app/build.rs"
cargo generate-lockfile --offline --manifest-path "$build_script/Cargo.toml"
expect_failure "$build_script" 'workspace generated-code target is not allowed:'
cargo check --offline --manifest-path "$build_script/Cargo.toml" >/dev/null 2>&1

echo "==> unknown and inline-assembly macros fail and compile"
for macro_case in unknown-macro inline-assembly; do
  macro_fixture="$scratch/$macro_case"
  new_fixture "$macro_fixture"
  if [ "$macro_case" = unknown-macro ]; then
    printf '%s\n' \
      'macro_rules! inert { () => { 1 }; }' \
      'pub fn value() -> i32 { inert!() }' >"$macro_fixture/crates/app/src/lib.rs"
  else
    printf '%s\n' \
      'pub unsafe fn machine_code() {' \
      '    core::arch::asm!("nop");' \
      '}' >"$macro_fixture/crates/app/src/lib.rs"
  fi
  cargo generate-lockfile --offline --manifest-path "$macro_fixture/Cargo.toml"
  cargo check --offline --manifest-path "$macro_fixture/Cargo.toml" >/dev/null 2>&1
  expect_failure "$macro_fixture" 'direct socket API:'
done

echo "==> macro definitions, authority aliases, and Unicode macros fail and compile"
for macro_case in shadow-format aliased-assembly unicode-macro; do
  macro_fixture="$scratch/$macro_case"
  new_fixture "$macro_fixture"
  case "$macro_case" in
    shadow-format)
      # The single-quoted values are literal Rust macro metavariables.
      # shellcheck disable=SC2016
      printf '%s\n' \
        'macro_rules! format {' \
        '    ($root:ident, $module:ident, $kind:ident) => {' \
        '        $root::$module::$kind::connect("127.0.0.1:9")' \
        '    };' \
        '}' \
        'pub fn connect() { let _ = format!(std, net, TcpStream); }' \
        >"$macro_fixture/crates/app/src/lib.rs"
      ;;
    aliased-assembly)
      printf '%s\n' \
        'use core::arch::asm as format;' \
        'pub unsafe fn machine_code() { unsafe { format!("nop") }; }' \
        >"$macro_fixture/crates/app/src/lib.rs"
      ;;
    unicode-macro)
      printf '%s\n' \
        'macro_rules! réseau { () => { 1 }; }' \
        'pub fn value() -> i32 { réseau!() }' \
        >"$macro_fixture/crates/app/src/lib.rs"
      ;;
  esac
  cargo generate-lockfile --offline --manifest-path "$macro_fixture/Cargo.toml"
  cargo check --offline --manifest-path "$macro_fixture/Cargo.toml" >/dev/null 2>&1
  expect_failure "$macro_fixture" 'direct socket API:'
done

echo "==> harmless extern blocks do not capture later function bodies"
harmless_extern="$scratch/harmless-extern"
new_fixture "$harmless_extern"
printf '%s\n' \
  'unsafe extern "C" { fn harmless(); }' \
  'pub fn connect() {' \
  '    let callback = || 1;' \
  '    let _ = callback();' \
  '}' >"$harmless_extern/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$harmless_extern/Cargo.toml"
cargo check --offline --manifest-path "$harmless_extern/Cargo.toml" >/dev/null 2>&1
run_gate "$harmless_extern"

echo "==> proc macros and out-of-package targets fail and compile"
proc_macro_fixture="$scratch/proc-macro"
new_fixture "$proc_macro_fixture"
printf '%s\n' '' '[lib]' 'proc-macro = true' >>"$proc_macro_fixture/crates/app/Cargo.toml"
printf '%s\n' \
  'extern crate proc_macro;' \
  'use proc_macro::TokenStream;' \
  '#[proc_macro]' \
  'pub fn inert(_: TokenStream) -> TokenStream { TokenStream::new() }' \
  >"$proc_macro_fixture/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$proc_macro_fixture/Cargo.toml"
cargo check --offline --manifest-path "$proc_macro_fixture/Cargo.toml" >/dev/null 2>&1
expect_failure "$proc_macro_fixture" 'workspace generated-code target is not allowed:'

outside_target="$scratch/outside-target"
new_fixture "$outside_target"
printf '%s\n' '' '[lib]' 'path = "../../external.rs"' \
  >>"$outside_target/crates/app/Cargo.toml"
printf '%s\n' 'pub fn local_only() {}' >"$outside_target/external.rs"
cargo generate-lockfile --offline --manifest-path "$outside_target/Cargo.toml"
cargo check --offline --manifest-path "$outside_target/Cargo.toml" >/dev/null 2>&1
expect_failure "$outside_target" 'Cargo target source is outside its package:'

echo "==> malformed Rust fails closed"
malformed="$scratch/malformed"
new_fixture "$malformed"
printf '%s\n' '/* unterminated' >"$malformed/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$malformed/Cargo.toml"
expect_failure "$malformed" 'could not parse Rust source'

unterminated_char="$scratch/unterminated-char"
new_fixture "$unterminated_char"
printf '%s\n' "pub fn malformed() { let value = 'x; }" \
  >"$unterminated_char/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$unterminated_char/Cargo.toml"
expect_failure "$unterminated_char" 'unterminated character literal'

echo "==> grouped and aliased libc socket imports fail"
libc_alias="$scratch/libc-alias"
libc_dependency="$scratch/dependencies/libc-shim"
new_fixture "$libc_alias"
new_dependency "$libc_dependency" libc
printf '%s\n' '' '[dependencies]' \
  'libc = { path = "../../../dependencies/libc-shim" }' \
  >>"$libc_alias/crates/app/Cargo.toml"
printf '%s\n' \
  'use libc::{connect as dial, socket as open_socket};' \
  'pub fn aliases() { let _ = (dial, open_socket); }' \
  >"$libc_alias/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$libc_alias/Cargo.toml"
expect_failure "$libc_alias" 'direct socket API:'

echo "no-network fixture tests passed"
