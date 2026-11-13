#!/usr/bin/env bash
# Build and install the shell without changing the running desktop session.
set -euo pipefail

usage() {
    cat <<'HELP'
Usage: scripts/install.sh [--prefix DIRECTORY]

Build a locked release and install shoji-bar-4 into DIRECTORY/bin.
The default prefix is $HOME/.local. Run again to update the installed binary.

Options:
  --prefix DIRECTORY  Choose the installation prefix (no sudo is invoked).
  -h, --help          Show this help.

Requires Cargo, a Rust toolchain, and the project's native build dependencies.
Does not install system packages, edit compositor settings, or restart the shell.
HELP
}

fail() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

prefix=''
while (($#)); do
    case "$1" in
        --prefix)
            (($# >= 2)) || fail '--prefix requires a directory'
            [[ -n "$2" && "$2" != -* ]] || fail '--prefix requires a directory'
            prefix=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *) fail "unknown argument: $1 (see --help)" ;;
    esac
done

command -v cargo >/dev/null 2>&1 || fail 'Cargo is missing; install a Rust toolchain first (https://rustup.rs).'
if [[ -z "$prefix" ]]; then
    [[ -n "${HOME:-}" ]] || fail 'HOME is unset; pass --prefix DIRECTORY'
    prefix="$HOME/.local"
fi
# Resolve relative prefixes before changing to the checkout for Cargo configuration.
case "$prefix" in
    /*) ;;
    *) prefix="$PWD/$prefix" ;;
esac
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"

printf 'Building and installing to %s/bin/shoji-bar-4\n' "$prefix"
(
    cd -- "$repo_dir"
    cargo install --path "$repo_dir" --locked --force --root "$prefix" --bin shoji-bar-4
)

printf '\nInstalled: %s/bin/shoji-bar-4\n' "$prefix"
case ":${PATH:-}:" in
    *":$prefix/bin:"*) ;;
    *) printf 'For terminal commands, add this directory to PATH: %s/bin\n' "$prefix" ;;
esac
printf 'Point your compositor shell command and keybindings at this executable.\n'
printf 'The running shell was not restarted; use the new binary next session.\n'
