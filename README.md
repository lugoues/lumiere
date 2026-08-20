# Lumière

Lumière is a daemon and browser UI for controlling Neewer lights over Bluetooth Low Energy. It is a Rust rewrite of [NeewerLux](https://github.com/poizenjam/NeewerLux) with a simulator, presets, and animation playback.

## Quick start

On macOS, a future personal Homebrew tap can be installed with these placeholders:

```sh
brew tap lugoues/tap
brew install lugoues/tap/lumiere
brew services start lumiere   # no sudo: Bluetooth needs a user LaunchAgent
```

To build a release archive from source, install the tools configured by `mise`, then run:

```sh
cargo xtask dist
```

For development without Bluetooth hardware, run the daemon against its simulated lights:

```sh
cargo run -p lumiere-daemon -- --sim
```

The daemon prints its API token and a bootstrap URL such as `http://127.0.0.1:9091/#t=TOKEN`. Open that URL once. The UI saves the token in browser storage and removes it from the address bar. The token is also available in `config.toml`.

**On macOS, always use `brew services start lumiere` without `sudo`. Bluetooth TCC permissions require a user LaunchAgent. Using sudo creates a LaunchDaemon, which macOS does not grant Bluetooth access.**

## Verifying releases

Every release artifact carries keyless sigstore provenance signed by the
GitHub Actions workflow. To check that a download really came from this
repo's release pipeline:

```sh
gh attestation verify lumiere-0.1.1-aarch64-apple-darwin.tar.gz --repo lugoues/lumiere
```

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

## REST API

Everything the UI does goes through the HTTP API, so scripts and home
automation can drive the lights directly. All routes live under `/api/v1` and
take JSON. Authentication is a bearer token from `config.toml`; export it once:

```sh
TOKEN="..."   # from the config file the daemon prints at startup
AUTH="Authorization: Bearer $TOKEN"
BASE="http://127.0.0.1:9091/api/v1"
```

With `disable_authentication = true` (or `--disable-authentication`), drop the
header entirely.

```sh
# Lights: current world state, ids come from here
curl -H "$AUTH" $BASE/lights

# Scan for new lights (10 seconds)
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"duration_ms": 10000}' $BASE/scan

# Set every light to 4200 K at 60%; wait:true returns per-light outcomes
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"selector": {"kind": "all"}, "mode": {"mode": "cct", "temp": 4200, "bri": 60}, "wait": true}'   $BASE/command

# One light to a color (ids are percent-encoded in paths: sim:1 -> sim%3A1)
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"selector": {"kind": "ids", "ids": ["sim:1"]}, "mode": {"mode": "hsi", "hue": 300, "sat": 100, "bri": 80}}'   $BASE/command

# Power is a mode too
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"selector": {"kind": "all"}, "mode": {"mode": "off"}}' $BASE/command

# Presets: list, recall, capture new, overwrite existing
curl -H "$AUTH" $BASE/presets
curl -X POST -H "$AUTH" -H 'Content-Type: application/json' -d '{"wait": true}' $BASE/presets/daylight/recall
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"name": "Evening", "selector": {"kind": "all"}}' $BASE/presets
curl -X POST -H "$AUTH" -H 'Content-Type: application/json' -d '{}' $BASE/presets/evening/capture

# Animations: list, play with options, stop
curl -H "$AUTH" $BASE/animations
curl -X POST -H "$AUTH" -H 'Content-Type: application/json'   -d '{"options": {"speed": 1.0, "fps": 5, "bri_scale": 1.0}}' $BASE/animations/police-flash/play
curl -X POST -H "$AUTH" $BASE/playback/stop
```

Command results are honest per light: `applied` means the bytes reached the
light, `adapted` means the request was adjusted to the light's abilities
(temperature clamped to its range, or color converted to a temperature on a
bi-color light) with both values reported, `skipped` and `failed` say why.

For live state, subscribe to the WebSocket at `/api/v1/events`: fetch a
single-use ticket from `POST $BASE/ws-ticket`, connect, and send
`{"t": "hello", "protocol_version": 1, "ticket": "...", "last_seq": null}`.
You get a full snapshot, then incremental patches. Polling `GET $BASE/lights`
works fine for scripts that do not need push updates.

## Testers wanted

Lumière is developed against two NEEWER-GL1 PRO lights on macOS and Linux.
Reports from other setups are what harden it:

- **Windows**: the code compiles and ships for `x86_64-pc-windows-msvc`, but no
  one has run the Bluetooth path on real Windows hardware yet. If you have a
  Windows machine with a BLE adapter, run `lumiere probe scan`, `probe blink
  <name>`, and `probe bench <name>` and open an issue with the output, working
  or not.
- **Other Neewer models**: the capability table covers 43 models but most are
  untested against real hardware. If a light shows the wrong temperature range,
  refuses color, or misbehaves in animations, open an issue with the light's
  advertised name from `lumiere probe scan`.

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
