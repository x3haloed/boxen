#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# The pinned build helper verifies its compiler download and builds both architectures.
# Override COSMO_HOME to reuse a toolchain cache across projects.
export COSMO_HOME="${COSMO_HOME:-$PWD/work/cosmo}"
cargo build --locked --release --features ape
test -s target/cosmo/release/boxen.com
printf '%s\n' 'Built target/cosmo/release/boxen.com'
