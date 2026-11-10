# Distributing Ariadne

Releases are cut from GitHub Actions. Pushing a version tag builds
unsigned native packages for macOS, Windows, and Linux and attaches them
to a GitHub Release.

## Cutting a release

1. Bump the version in `app/Cargo.toml` (then `cargo build` in `app/` to
   refresh `Cargo.lock`).
2. Commit and push to `main`.
3. Tag and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
   The tag must equal the `Cargo.toml` version or the workflow fails fast.
4. One-time repo setup: add the `GOOGLE_CLIENT_ID` secret (Settings →
   Secrets and variables → Actions). It is baked into release builds as
   `ARIADNE_GOOGLE_CLIENT_ID` and enables Settings → Sync → Connect
   Google Drive. Builds without it still work; Google Drive just shows as
   unconfigured. The secret is a public OAuth client ID (Desktop app type,
   PKCE, no client secret) — safe to embed, but kept out of the repo.
4. Watch the `Release` workflow; when green, the GitHub Release page has:
   - macOS: `ariadne-<version>-macos-universal.dmg` containing
     `Ariadne.app` (arm64 + x86_64 universal, unsigned — right-click →
     Open on first launch)
   - Windows: `Ariadne-Setup-<version>-x64.exe` (Inno Setup installer,
     unsigned — SmartScreen will warn: "More info" → "Run anyway")
   - Linux: `ariadne_<version>-1_amd64.deb` (binary, icon, `.desktop`
     entry) plus a portable `ariadne-<version>-linux-x86_64.tar.gz`
5. Every build job smoke-tests its binary first (launch + `--screenshot`,
   assert a non-blank PNG), so broken artifacts never reach the release.

## How it works

- `.github/workflows/release.yml` — tag-triggered matrix. Each OS builds
  natively (no cross-compiling) and packages the platform way:
  - macOS: two `cargo build --release` runs (`aarch64` + `x86_64`) fused
    with `lipo`, bundled as `Ariadne.app` (`dist/macos/Info.plist`,
    `app/assets/icon.icns`), wrapped in a DMG via `hdiutil`.
  - Windows: `cargo build --release` (embeds `app/assets/icon.ico` via
    `app/build.rs` + `winres`), installer compiled by Inno Setup from
    `dist/windows/app.iss`.
  - Linux: `cargo deb` (metadata in `app/Cargo.toml`
    `[package.metadata.deb]`, installs binary + `.desktop` + icon)
    plus a portable tarball.
- `.github/workflows/ci.yml` — every push/PR: `cargo test` and the
  `cargo-deny` license/advisory audit (`deny.toml`).
- No code signing yet: adding it later is config + secrets only
  (Apple Developer ID + notarization, Authenticode cert) — no code changes.

## Local release builds

```sh
cd app
cargo build --release          # native binary
cargo deb                      # Debian package (Linux only)
cargo deny check               # license + advisory audit
```
