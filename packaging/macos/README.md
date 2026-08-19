# macOS release checklist

- Start the service with `brew services start lumiere`, without `sudo`.
- If `cargo run` works but the service does not, check macOS Bluetooth TCC permissions. The service must run as a user LaunchAgent.
- Verify the embedded usage description with `otool -s __TEXT __info_plist /path/to/lumiere-daemon`.
