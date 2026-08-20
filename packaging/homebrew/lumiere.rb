# Bluetooth TCC requires a LaunchAgent: use brew services WITHOUT sudo. With
# sudo Homebrew creates a LaunchDaemon, which macOS never grants Bluetooth.
class Lumiere < Formula
  desc "Daemon and web UI for controlling Neewer BLE lights"
  homepage "https://github.com/lugoues/lumiere"
  url "https://github.com/lugoues/lumiere/releases/download/vVERSION/lumiere-VERSION-aarch64-apple-darwin.tar.gz"
  sha256 "SHA256"
  version "VERSION"

  def install
    bin.install "lumiere-daemon"
    bin.install "lumiere"
  end

  service do
    run [opt_bin/"lumiere-daemon"]
    keep_alive true
    log_path var/"log/lumiere.log"
    error_log_path var/"log/lumiere.log"
    environment_variables PATH: std_service_path_env,
                          LUMIERE_CONFIG_DIR: var/"lumiere/config",
                          LUMIERE_DATA_DIR: var/"lumiere/data"
  end

  test do
    assert_match "lumiere probe", shell_output("#{bin}/lumiere 2>&1", 1)
  end
end
