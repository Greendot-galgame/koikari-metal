#!/bin/sh
# Keep every Cargo write inside the project directory.
#
# The game lives on an external volume whose parent directories are not writable
# by the sandboxed agent processes, so the shared ~/.cargo registry cannot be
# populated. Pointing CARGO_HOME at a project-local directory keeps dependency
# downloads, the registry cache and the target directory inside the workspace.
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export CARGO_HOME="$PROJECT_DIR/.cargo-home"
export CARGO_TARGET_DIR="$PROJECT_DIR/target"
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR"
cd "$PROJECT_DIR"
exec cargo "$@"
