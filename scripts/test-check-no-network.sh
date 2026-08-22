#!/usr/bin/env bash
# Fixture tests for scripts/check-no-network.sh.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
gate="$script_dir/check-no-network.sh"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/phantom-no-network-tests.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

new_fixture() {
  fixture=$1
  mkdir -p "$fixture/crates/app/src"
  printf '%s\n' \
    '[workspace]' \
    'members = ["crates/app"]' \
    'resolver = "2"' >"$fixture/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    'name = "fixture-app"' \
    'version = "0.1.0"' \
    'edition = "2021"' >"$fixture/crates/app/Cargo.toml"
  printf '%s\n' 'pub fn local_only() {}' >"$fixture/crates/app/src/lib.rs"
}

run_gate() {
  PHANTOM_NO_NETWORK_ROOT=$1 "$gate"
}

echo "==> clean fixture passes"
clean="$scratch/clean"
new_fixture "$clean"
cargo generate-lockfile --offline --manifest-path "$clean/Cargo.toml"
run_gate "$clean"

echo "==> renamed HTTP dependency fails"
renamed="$scratch/renamed"
dependency="$scratch/dependencies/reqwest-shim"
new_fixture "$renamed"
mkdir -p "$dependency/src"
printf '%s\n' \
  '[package]' \
  'name = "reqwest"' \
  'version = "99.0.0"' \
  'edition = "2021"' >"$dependency/Cargo.toml"
printf '%s\n' 'pub fn request() {}' >"$dependency/src/lib.rs"
printf '%s\n' \
  '[package]' \
  'name = "fixture-app"' \
  'version = "0.1.0"' \
  'edition = "2021"' \
  '' \
  '[dependencies]' \
  'transport = { package = "reqwest", path = "../../../dependencies/reqwest-shim" }' \
  >"$renamed/crates/app/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$renamed/Cargo.toml"
if output=$(run_gate "$renamed" 2>&1); then
  echo "renamed HTTP dependency unexpectedly passed" >&2
  exit 1
fi
echo "$output" | grep -q 'resolved package: reqwest'

echo "==> direct std::net use fails"
socket="$scratch/socket"
new_fixture "$socket"
printf '%s\n' \
  'pub fn connect() {' \
  '    let _ = std::net::TcpStream::connect("127.0.0.1:9");' \
  '}' >"$socket/crates/app/src/lib.rs"
cargo generate-lockfile --offline --manifest-path "$socket/Cargo.toml"
if output=$(run_gate "$socket" 2>&1); then
  echo "direct std::net use unexpectedly passed" >&2
  exit 1
fi
echo "$output" | grep -q 'uses a direct socket API'

echo "no-network fixture tests passed"
