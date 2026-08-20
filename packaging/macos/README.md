# macOS packaging

## Releasing

1. Tag and push: `git tag v0.1.0 && git push origin v0.1.0`.
2. The Release workflow builds `lumiere-<version>-aarch64-apple-darwin.tar.gz`
   plus a ready `lumiere.rb` formula with the sha256 filled in, attached to the
   GitHub release.

## Setting up the tap (once)

1. Create the repo `github.com/lugoues/homebrew-tap` (empty is fine).
2. Create a fine-grained PAT with contents read/write on that repo and add it
   to the lumiere repo as the `TAP_GITHUB_TOKEN` actions secret. Every release
   then pushes `Formula/lumiere.rb` into the tap automatically; without the
   secret, the workflow skips that step and the formula is still attached to
   the release for manual copying.
3. Install:
   ```
   brew tap lugoues/tap
   brew install lugoues/tap/lumiere
   brew services start lumiere    # NO sudo, see below
   ```
On new releases: `brew upgrade lumiere` (the tap already has the new formula).

Note: if the lumiere repo is private, `brew install` cannot download the
tarball anonymously. Either make the repo public or export
`HOMEBREW_GITHUB_API_TOKEN` with a token that can read releases.

## Bluetooth checklist

- Start the service with `brew services start lumiere`, without `sudo`.
  Bluetooth TCC only prompts for user LaunchAgents; with `sudo` Homebrew
  creates a LaunchDaemon, which macOS never grants Bluetooth.
- If running the binary in a terminal works but the service finds no lights,
  it is the TCC grant: check System Settings > Privacy & Security > Bluetooth.
- Verify the embedded usage description with
  `otool -s __TEXT __info_plist $(brew --prefix)/bin/lumiere-daemon`.
- Config and data live under `$(brew --prefix)/var/lumiere/`; logs at
  `$(brew --prefix)/var/log/lumiere.log`. The API token is printed to the log
  on first start.
