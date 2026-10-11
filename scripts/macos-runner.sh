#!/bin/sh
# Cargo runner on macOS (see .cargo/config.toml), used by `cargo run` and `cargo test`.
#
# An unsigned binary gets a new identity on every build, so the macOS keychain asks again
# for permission to read the saved token after each rebuild. Signing the binary with a
# local code-signing certificate gives every build the same identity, and "Always Allow"
# then sticks. See CONTRIBUTING.md for how to create the certificate.
#
# Without that certificate, the binary runs unchanged.
bin="$1"
if [ "$(basename "$bin")" = "lazyforge" ]; then
    "$(dirname "$0")/sign-dev.sh" "$bin" --quiet
fi
exec "$@"
