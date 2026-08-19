# Lumière

Lumière is a daemon and browser UI for controlling Neewer lights over Bluetooth Low Energy. It is a Rust rewrite of [NeewerLux](https://github.com/poizenjam/NeewerLux) with a simulator, presets, and animation playback.

## Quick start

On macOS, a future personal Homebrew tap can be installed with these placeholders:

```sh
brew tap OWNER/TAP
brew install OWNER/TAP/lumiere
brew services start lumiere
```

To build a release archive from source, install the tools configured by `mise`, then run:

```sh
cargo xtask dist
```

For development without Bluetooth hardware, run the daemon against its simulated lights:

```sh
cargo run -p lumiere-daemon -- --sim
```

The daemon prints its API token and a bootstrap URL such as `http://127.0.0.1:8080/#t=TOKEN`. Open that URL once. The UI saves the token in browser storage and removes it from the address bar. The token is also available in `config.toml`.

**On macOS, always use `brew services start lumiere` without `sudo`. Bluetooth TCC permissions require a user LaunchAgent. Using sudo creates a LaunchDaemon, which macOS does not grant Bluetooth access.**

## Hardware probe

The `lumiere` binary includes focused tools for checking BLE discovery, identifying a light, and benchmarking writes:

```sh
lumiere probe scan --seconds 10
lumiere probe blink <id-or-name-fragment> --seconds 3
lumiere probe bench <id-or-name-fragment> --writes 100
```

## Configuration and data

Lumière follows the operating system's standard per-user directories:

| Platform | Configuration | Data |
| --- | --- | --- |
| macOS | `~/Library/Application Support/lumiere/config.toml` | `~/Library/Application Support/lumiere/` |
| Linux | `~/.config/lumiere/config.toml` | `~/.local/share/lumiere/` |

Set `LUMIERE_CONFIG_DIR` or `LUMIERE_DATA_DIR` to override those directories. The data directory contains light labels, presets, and animations. Development builds serve the UI from `dist/web`; `LUMIERE_WEB_ROOT` overrides that path when the `embed-ui` feature is off.

## Development

- `cargo xtask ui [--debug]` builds the Dioxus web UI and synchronizes it to `dist/web`.
- `cargo xtask dist` builds the release UI and embedded daemon, builds the CLI, and creates a release archive.
- `cargo xtask convert-anims` converts the reference NeewerLux animations.
- `cargo xtask dump-schedule FILE` emits schedule frames for comparison with `assets/dev/diff_engine.py`.
- `cargo test --workspace` runs the workspace tests.

Real BLE access inside the devcontainer needs the Linux host's BlueZ system D-Bus socket. The optional bind mount is documented in `.devcontainer/devcontainer.json`; enabling it on non-Linux hosts can prevent the container from starting.

## License and credits

The license is the same as upstream. See [NeewerLite-Python](https://github.com/taburineagle/NeewerLite-Python) for license details.

Lumière builds on the work of [NeewerLux](https://github.com/poizenjam/NeewerLux), [NeewerLite-Python](https://github.com/taburineagle/NeewerLite-Python) by Zach Glenwright, and [NeewerLite](https://github.com/keefo/NeewerLite) by Xu Lian.
