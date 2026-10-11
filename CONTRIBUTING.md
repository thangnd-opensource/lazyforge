# Contributing

```sh
cargo run          # needs `lazyforge auth login` first, or GITLAB_TOKEN
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Pull requests run the same checks in CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)): formatting, clippy, the tests on Linux, macOS and Windows, and a build with the oldest supported Rust (`rust-version` in Cargo.toml).

See the "Development" section of the [README](README.md) for how the code is organised.

## macOS: stop the keychain asking after every build

`lazyforge auth login` saves the token in the macOS Keychain, which only lets the app that
saved it read it without asking. A locally built binary is not signed, so it looks like a
new app after every rebuild and macOS asks again ("lazyforge wants to use your confidential
information…").

Sign your builds with a self-signed certificate and every build keeps the same identity:

1. Open **Keychain Access**. In the menu bar: **Keychain Access → Certificate Assistant →
   Create a Certificate…**
2. Name: `lazyforge-dev`. Identity Type: **Self Signed Root**. Certificate Type: **Code Signing**.
   Click **Create**, then **Done**.
3. That's it: `cargo run` now signs the binary before starting it (see
   `.cargo/config.toml` and `scripts/macos-runner.sh`). For a binary you run directly, such
   as `target/release/lazyforge`, sign it after building:

   ```sh
   cargo build --release && scripts/sign-dev.sh
   ```

4. The next time macOS asks, click **Always Allow**. It won't ask again for signed builds.

Check that a binary is signed with `codesign -dv target/release/lazyforge` (look for
`Authority=lazyforge-dev` and `Identifier=dev.lazyforge`). Use another certificate name with
`LAZYFORGE_SIGN_IDENTITY=...`.

The certificate is only for your machine; it does not make the binary trusted anywhere else.
Released binaries will need a proper Developer ID signature (see ROADMAP.md).

## Making a release

With [cargo-release](https://github.com/crate-ci/cargo-release) (`cargo install cargo-release`, once), configured in [release.toml](release.toml):

```sh
cargo release patch             # dry run: shows what would happen, changes nothing
cargo release patch --execute   # 0.1.0 → 0.1.1 (`minor`: 0.2.0, `major`: 1.0.0)
```

On a clean `main`, this bumps `version` in Cargo.toml and Cargo.lock, runs the CI checks (fmt, clippy, tests; a failure stops it before anything is committed), commits "Release 0.1.1", tags `v0.1.1`, and pushes both. It does not publish to crates.io.

Pushing the tag starts [.github/workflows/release.yml](.github/workflows/release.yml): it checks that the tag matches the version, builds Linux (x86_64, ARM64), macOS (Apple Silicon, Intel) and Windows binaries, and attaches them, with a SHA-256 checksum each, to a **draft** release. Open it under Releases, check the files and notes, and press **Publish release**.
