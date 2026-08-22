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
  printf '%s\n' \
    '[workspace]' \
    'members = ["crates/app"]' \
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
with open(path, "w", encoding="utf-8") as metadata_file:
    json.dump(metadata, metadata_file)
PY
if output=$(python3 "$script_dir/check-no-network.py" "$feature_metadata" 2>&1); then
  echo "unreviewed egui-winit features unexpectedly passed" >&2
  exit 1
fi
grep -q 'unreviewed features for egui-winit' <<<"$output"

echo "==> harmless address types, comments, and strings pass"
clean="$scratch/clean"
new_fixture "$clean"
printf '%s\n' \
  'pub const EXAMPLE: &str = "std::net::TcpStream::connect";' \
  'pub const ATTRIBUTE_EXAMPLE: &str = r##"#[link_name = "socket"]"##;' \
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
expect_failure "$unknown" 'unreviewed direct dependency identity: phantom-core -> innocent-wrapper'

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
