#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# The pinned build helper verifies its compiler download and builds both architectures.
# Override COSMO_HOME to reuse a toolchain cache across projects.
export COSMO_HOME="${COSMO_HOME:-$PWD/work/cosmo}"
# Use the official release asset for the compiler pinned by cosmo-build 10.0.0.
# The helper checks its SHA-256 regardless of the download location.
export COSMO_COSMOCC_URL="${COSMO_COSMOCC_URL:-https://github.com/jart/cosmopolitan/releases/download/4.0.2/cosmocc-4.0.2.zip}"
cargo build --locked --release --features ape
test -s target/cosmo/release/boxen.com
printf '%s\n' 'Built target/cosmo/release/boxen.com'
