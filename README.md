# openLab

openLab is an open-source show-playback system for theatre, in the spirit of QLab 5. A headless Rust engine owns the
show, timing and audio; a native Swift/AppKit app on macOS is the operator interface, so a UI fault never stops
playback. See [`REQUIREMENTS.md`](REQUIREMENTS.md), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
[`PROGRESS.md`](PROGRESS.md).

## Licence

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

## Building

Prerequisites (macOS 12 or later to run; building needs a Mac with Xcode):

- [rustup](https://rustup.rs) — the pinned toolchain in `engine/rust-toolchain.toml` installs automatically
- Xcode 27.x
- [XcodeGen](https://github.com/yonaskolb/XcodeGen): `brew install xcodegen`
- For development: `rustup toolchain install nightly`, `cargo install cargo-fuzz cargo-deny`

```
make lint        # cargo fmt --check, clippy -D warnings, cargo deny, swift format lint
make test        # all Rust tests + macOS unit and UI tests
make test-rust   # Rust tests only (also works on Windows with cargo)
make test-mac    # generate the Xcode project and run macOS unit and UI tests
make mac         # build the Release app with a universal engine: build/Build/Products/Release/openLab.app
```

The Xcode project is generated from `mac/project.yml` and is not committed; run `cd mac && xcodegen` to open it in
Xcode. UI tests need macOS Automation Mode; Xcode asks for authorisation the first time they run.
