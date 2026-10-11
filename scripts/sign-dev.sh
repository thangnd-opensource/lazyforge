#!/bin/sh
# Signs lazyforge binaries with a local code-signing certificate (macOS only), so the keychain
# sees every build as the same app. Usage:
#
#   scripts/sign-dev.sh                       # signs target/debug/lazyforge and target/release/lazyforge
#   scripts/sign-dev.sh path/to/lazyforge        # signs one binary
#
# The certificate name defaults to "lazyforge-dev"; set LAZYFORGE_SIGN_IDENTITY to use another.
set -e
identity="${LAZYFORGE_SIGN_IDENTITY:-lazyforge-dev}"
quiet=false
binaries=""
for arg in "$@"; do
    case "$arg" in
        --quiet) quiet=true ;;
        *) binaries="$binaries $arg" ;;
    esac
done
if [ -z "$binaries" ]; then
    root="$(cd "$(dirname "$0")/.." && pwd)"
    binaries="$root/target/debug/lazyforge $root/target/release/lazyforge"
fi

[ "$(uname)" = "Darwin" ] || exit 0
if ! security find-identity -p codesigning 2>/dev/null | grep -q "\"$identity\""; then
    $quiet || echo "No code-signing certificate named \"$identity\" (see CONTRIBUTING.md); binaries left unsigned." >&2
    exit 0
fi

for bin in $binaries; do
    [ -f "$bin" ] || continue
    # The identifier is part of the identity macOS remembers, so keep it fixed.
    if codesign --force --sign "$identity" --identifier dev.lazyforge "$bin" 2>/tmp/lazyforge-sign.err; then
        $quiet || echo "Signed $bin with \"$identity\"."
    else
        echo "Could not sign $bin: $(cat /tmp/lazyforge-sign.err)" >&2
    fi
done
