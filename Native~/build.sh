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

# Only the artifact this invocation actually produces gets published. Copying
# whatever happens to be lying in the output directory silently republishes
# stale libraries: a Windows build and a WSL build share one target/release, so
# the other platform's library would be copied out again long after it stopped
# matching the source.
case "${target:-$(uname -s)}" in
    *windows*|*MINGW*|*MSYS*|*CYGWIN*) expected=mirror_wtransport.dll ;;
    *apple*|*darwin*|Darwin)           expected=libmirror_wtransport.dylib ;;
    *)                                 expected=libmirror_wtransport.so ;;
esac

if [ ! -f "$out_dir/$expected" ]; then
    echo "cargo reported success but $expected is not in $out_dir" >&2
    exit 1
fi

mkdir -p "$plugins"
cp -f "$out_dir/$expected" "$plugins/$expected"
echo "copied $expected to Runtime/Plugins/x86_64"

# Point out the other platforms' libraries when they have fallen behind, since
# nothing else will notice until a dedicated server build fails the ABI check.
for other in mirror_wtransport.dll libmirror_wtransport.so libmirror_wtransport.dylib; do
    [ "$other" = "$expected" ] && continue
    if [ -f "$plugins/$other" ] && [ "$plugins/$other" -ot "$plugins/$expected" ]; then
        echo "warning: $other in Runtime/Plugins/x86_64 is older than the library just built. Rebuild it for that platform before shipping." >&2
    fi
done

echo "done. Unity will import it on the next domain reload."
