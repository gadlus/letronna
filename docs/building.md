# Building

```sh
cargo build --release
```

This produces `target/release/letronna` (the terminal) and `letronna-gui` (the window).

## Requirements

- **Windows**: Rust, the Visual Studio C++ build tools, and a Windows SDK that includes `fxc.exe`. Point `GPUI_FXC_PATH` in `.cargo/config.toml` at the SDK's shader compiler.
- **Linux**: GPUI's usual dependencies: Vulkan, X11 or Wayland, and `libxkbcommon`.
- **macOS**: the Command Line Tools are enough. The build uses GPUI's `runtime_shaders`, so Xcode's Metal toolchain is not needed. Run `scripts/bundle-macos.sh` after a release build to get `target/Letronna.app`.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release
```

The golden prompt tests compare the system prompt against recorded files. After an intended prompt change, re-record them:

```sh
LETRONNA_GOLDEN_RECORD=1 cargo test -p letronna-core golden
```

## UI changes

Run the app and look at the change. `scripts/shot.ps1` captures the terminal window, and `scripts/verify-ui.ps1 -Action capture|click|send` drives the GUI.

## Icons

Icons are Phosphor Light. `scripts/icons.sh` re-syncs them from npm.

## Fonts

The window brings its type: Inter, JetBrains Mono and Newsreader, in `crates/gui/assets/fonts`. `scripts/fonts.sh` fetches them again and cuts them down; it needs `fonttools`.

## Releases

Releases are tagged `vX.Y.Z`. CI builds every platform into a draft. `scripts/release.ps1 sign <tag>` signs `latest.json` with an offline release key and publishes it. The full steps are in [CONTRIBUTING.md](../CONTRIBUTING.md#releasing).
