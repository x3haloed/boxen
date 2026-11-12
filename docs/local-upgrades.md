# Local Boxen upgrades

Use the script for the host OS from the checkout you want to install. It builds both artifacts with the repository's locked Cargo dependencies, checks the daemon executable, and stages both files before replacing either installed artifact. It uses the local checkout and its current changes. The operator chooses the checkout revision and the maintenance window.

## One-time path setup

| OS | Daemon payload | Desktop payload | Command from the repository root |
| --- | --- | --- | --- |
| macOS | `/usr/local/libexec/boxen/boxen` | `/Applications/Boxen.app` | `./scripts/upgrade-macos.sh` |
| Windows | `%ProgramFiles%\Boxen\boxen.exe` | `%ProgramFiles%\Boxen\boxen-desktop.exe` | `python .\scripts\upgrade_local.py win32` |
| Linux | `~/.local/bin/boxen.com` | `~/.local/bin/boxen-desktop` | `./scripts/upgrade-linux.sh` |

Create the install directories and point the existing daemon service and desktop shortcut at these locations once. Keep the service's existing account, arguments and data directory.

- On macOS, create the daemon directory with `sudo install -d -o root -g wheel -m 0755 /usr/local/libexec/boxen`. The daemon is native and the desktop is a locally ad-hoc-signed bundle. Run the upgrade as your normal build user. The script uses `sudo` for writes to the install directories.
- On Windows, use an elevated shell with `python` and the Rust MSVC build toolchain available. `powershell -File .\scripts\upgrade-windows.ps1` runs the same upgrade where PowerShell policy permits unsigned scripts. The new executables inherit permissions from the install directory, which should grant read and execute access to the desktop user and daemon service account. If the service uses a wrapper, configure the wrapper to run `boxen.exe` at the listed path.
- On Linux, launch the portable daemon through Bash, for example `/bin/bash %h/.local/bin/boxen.com --data-dir %h/.boxen serve` in a user systemd unit, with any existing service-specific arguments restored. Use the account that owns the Linux install.

All three scripts need Python 3 and the repo's Rust build prerequisites. The desktop needs Rust 1.94 or later and the platform graphics and windowing build dependencies. The Linux daemon build uses the existing [APE build script](../scripts/build-ape.sh). Its first run downloads the pinned toolchain. Set `COSMO_HOME` to reuse an existing Linux toolchain cache.

## Each upgrade

1. Wait for daemon work to finish. Stop the daemon through its service manager and fully quit the desktop.
2. Run the script above. Builds and checks complete before the installed payloads are touched. A replacement failure attempts to restore both previous payloads. If the script reports retained recovery files, inspect the named locations before starting either component. Each `.boxen-upgrade.*` staging directory sits beside its install target and may contain a `previous-` backup. After an abrupt termination, check for an abandoned `.boxen-upgrade-lock` directory beside the daemon. Remove the empty lock only after confirming the upgrade process has exited.
3. Start the daemon through its service manager and open the desktop normally.

The scripts only replace the two executable payloads. The existing service configuration, credentials, machine identity, journals and desktop state live separately. For the longer-term design, see [installation and upgrades](research/installation-and-upgrades.md).
