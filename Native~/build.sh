#!/usr/bin/env bash
# Builds the native WebTransport library and drops it where Unity picks it up.
#
#   ./Native~/build.sh                              # host target, release
#   ./Native~/build.sh --target aarch64-apple-darwin
#   ./Native~/build.sh --debug
#
# Requires a Rust toolchain (https://rustup.rs).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$root/mirror-wtransport"
plugins="$(dirname "$root")/Runtime/Plugins/x86_64"

target=""
profile="release"
cargo_args=(build --release)

while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            target="$2"
            shift 2
            ;;
        --debug)
            profile="debug"
            cargo_args=(build)
            shift
            ;;
        *)
            echo "unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [ -n "$target" ]; then
    cargo_args+=(--target "$target")
    out_dir="$crate/target/$target/$profile"
else
    out_dir="$crate/target/$profile"
fi

echo "building mirror-wtransport ($profile)..."
(cd "$crate" && cargo "${cargo_args[@]}")

mkdir -p "$plugins"

copied=0
for name in mirror_wtransport.dll libmirror_wtransport.so libmirror_wtransport.dylib; do
    if [ -f "$out_dir/$name" ]; then
        cp -f "$out_dir/$name" "$plugins/$name"
        echo "copied $name to Runtime/Plugins/x86_64"
        copied=1
    fi
done

if [ "$copied" -eq 0 ]; then
    echo "no library was produced in $out_dir" >&2
    exit 1
fi

echo "done. Unity will import it on the next domain reload."
