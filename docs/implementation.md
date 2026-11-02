# Implementation

[Back to README](../README.md) · [Configuration](configuration.md) · [Troubleshooting](troubleshooting.md)

## Components

Minohomo has three user interfaces over the same operations:

- The CLI manages sources, updates, configuration, and the running core.
- The Ratatui dashboard adds interactive navigation and background status/network checks.
- The StatusNotifier tray exposes live mode, proxy, and service controls.

Mihomo runs independently as the global `minohomo.service`. Closing the dashboard or disabling the tray does not stop the core. Subscription updates are manual. Mihomo may independently refresh rule providers if you configure them with update intervals.

## Files and ownership

| Path | Purpose |
| --- | --- |
| `~/.config/minohomo/config.yaml` | Client settings and base Mihomo configuration |
| `~/.config/minohomo/rules.yaml` | Complete local routing policy |
| `~/.config/minohomo/sources.yaml` | Saved links, IDs, names, timestamps, and cached nodes |
| `~/.config/minohomo/current.yaml` | Local generated config for validation and deployment |
| `~/.config/minohomo/GeoSite.dat` | Local geosite database |
| `/usr/share/minohomo/mihomo` | Package-owned bundled core (Debian/Arch) |
| `/var/lib/minohomo/mihomo` | Managed runtime executable |
| `/var/lib/minohomo/config.yaml` | Active runtime configuration |
| `/var/lib/minohomo/` | Runtime geodata and other Mihomo data |
| `/usr/lib/systemd/system/minohomo.service` | Global core unit |
| `/usr/lib/systemd/user/minohomo-tray*` | Packaged tray user units |

The per-user directory is mode 700; files Minohomo creates are mode 600. Source caches, configs, and backups may contain credentials. The runtime directory and binary are root-owned, mode 755; installed config and geodata remain owner-only. Runtime files belong under `/var/lib/minohomo`, not `/etc`.

`--config-dir` changes the per-user settings/source directory, not the runtime service or its destination. Relative `output` paths resolve against the selected profile. The local geosite database follows the output directory. Multiple profiles share one managed core and system service.

## Source updates and configuration

An `update` operation:

1. Fetches the selected subscription sources or parses direct links.
2. Combines fresh proxies with cached proxies from other sources.
3. Applies local proxy overrides, groups, and routing policy.
4. Copies the bundled core if the runtime executable is missing, or downloads one if both local copies are missing, then validates the candidate as the current user.
5. Saves the new local config and source state, keeping `current.yaml.bak` when replacing a config.
6. Invokes the same Minohomo executable through sudo with `apply`, unless a custom `reload-command` overrides deployment.

`update --no-sync` skips source fetching and parsing. It uses cached nodes without changing their timestamps. Sources without cached nodes contribute nothing; at least one cached node is required. Validation can still fetch missing geodata or rule-provider data, so this flag does not guarantee offline operation.

Subscription settings do not control the local network policy. Generation imports proxies, uses local rules and groups, replaces `proxies`, `rules`, `rule-providers`, and `geox-url.geosite`, and removes subscription proxy providers. Proxy overrides change the generated output, not cached source data.

A fetch, parse, or local validation failure preserves the saved local config and source state. If the privileged phase fails afterward, the new local config and cache remain saved for retry.

## Privileged apply

The default deployment invokes:

```sh
sudo minohomo apply --config /path/to/current.yaml --geosite /path/to/GeoSite.dat
```

The geosite argument is optional. The real invocation uses the current executable's path, not a shell command resolved from the user's settings.

The root helper does not load user settings or initialize root's profile. It accepts input paths and optional core download endpoints for non-bundled builds; the destination directory, managed validator, and service name are fixed.

The helper:

1. Checks input sizes and ensures the managed core and global unit exist.
2. Acquires the runtime lock under `/var/lib/minohomo`.
3. Snapshots input bytes into a staging directory alongside available runtime geodata.
4. Runs the managed core with `-t` against that snapshot.
5. Backs up and installs the validated config and relevant geodata.
6. Restarts `minohomo.service` through `/usr/bin/systemctl`.

System validation failure leaves the installed config and database unchanged. A restart failure leaves the new config installed and reports the error. Replacement files use temporary files and rename; this is not a single transaction across all files or the service restart.

Packaged builds copy `/usr/share/minohomo/mihomo` when the runtime core is missing. If both local copies are missing, apply downloads a core using `--core-version-url` and `--core-download-url`. Normal updates forward configured endpoints to privileged apply. Direct start/stop/restart fallback uses the built-in endpoints.

Treat apply as a privileged networking operation. Do not grant restricted passwordless sudo access on the assumption that arbitrary user-provided Mihomo configs are harmless. The unit runs as root with the capability settings in [packaging/minohomo.service](../packaging/minohomo.service).

## Bundled core and explicit updates

The default `bundled-core` feature downloads a pinned, SHA-256-checked release at compile time. It selects Cargo's target OS/architecture and uses `amd64-v1` for x86-64, regardless of the build machine's CPU features. The build caches the unpacked executable under `target/bundled-core/cache/`, keyed by target triple, pinned version, and archive checksum. This cache is shared across build profiles and build-script fingerprints. Debian and Arch install the resulting executable at `/usr/share/minohomo/mihomo`, mode 755. Compilation never runs the downloaded target executable, so cross-builds do not require emulation.

On first `update`, `apply`, `start`, `stop`, or `restart`, the client copies the bundle into `/var/lib/minohomo/mihomo` using privileged `install -D -m 755 -o root -g root`. It leaves the packaged copy untouched. If a runtime core already exists, it is preserved, including across Minohomo package upgrades. If both the runtime core and bundle are missing, the command runs the existing core updater automatically. An existing but unusable bundle still produces an error recommending package reinstallation or an explicit core update.

CLI/dashboard service controls use this setup path. First-use tray controls invoke the same copy-or-download setup through the root CLI using polkit's `pkexec`; later tray controls use systemctl as before. Starting the service still requires an applied configuration.

The Nix flake disables the default feature and keeps the existing download-on-first-use behavior. The rest of this section describes explicit core updates, which are unchanged in both builds.

`core version` queries only `/var/lib/minohomo/mihomo`. If it is absent, the command prints exactly `Not installed`. It does not initialize a profile or query a separate system installation.

`core update` reads a release tag from `core-version-url`, maps the host to an upstream artifact, and expands `core-download-url` using `{version}`, `{artifact}`, and `{ext}`. On x86-64, it selects `amd64-v2` only when the CPU supports that baseline; otherwise it uses `amd64-v1`.

The updater downloads and unpacks into a private `/tmp/minohomo-core-*` directory. It supports gzip executables and ZIP archives containing a Mihomo executable. Compressed downloads are limited to 128 MiB, unpacked executables to 256 MiB, and version metadata to 1 KiB. Requests use a 180-second timeout and a 15-second connection timeout. Core URLs and redirect destinations require HTTPS, except HTTP on loopback for local testing.

Before installation, the updater runs the unpacked binary with `-v` and checks its reported version against the requested release. This detects a wrong release or a binary that cannot run on the machine. It is not an independent cryptographic authenticity check. The endpoint supplies executable code; use trusted mirrors.

Normal-user updates download without root. Installation uses:

```text
sudo install -d -m 755 /var/lib/minohomo
sudo mv -- <unpacked-binary> /var/lib/minohomo/mihomo
sudo chown root:root /var/lib/minohomo/mihomo
sudo chmod 755 /var/lib/minohomo/mihomo
```

An already-root caller runs these commands without sudo. `mv` preserves source ownership, so the explicit `chown` matters. The updater installs the embedded global unit if missing, reloads systemd when creating that unit, and restarts the core only if the service is active. Package installation itself does not download the core or enable/start services.

Temporary files are cleaned on success and error. Download, unpack, and version-check failures preserve the installed binary. Installation or restart errors do not roll back a binary already moved into place. Errors include [manual recovery instructions](troubleshooting.md#core-download-or-installation-fails).

Minohomo itself remains a Unix/systemd application. Mapping a Windows release artifact does not add Windows dashboard or service support.

## Live API and network checks

The default controller is `127.0.0.1:9090`. Initialization creates a random secret in the base settings. Live commands read the controller address and secret from the last generated config, falling back to base settings if it does not exist. API requests bypass proxy environment variables.

Mode changes and proxy selection use the authenticated API, not config rewriting or service restarts. Persistent proxy selection follows Mihomo's `profile.store-selected`. A config reload can reset the mode to `mihomo.mode`.

Ping is an HTTP delay check through Mihomo, not ICMP. Checks use the configured top-level `ping-url`, defaulting to `https://www.gstatic.com/generate_204`, with a five-second per-entry timeout and up to eight concurrent checks. Results use running API names, sort successful checks fastest first, and place failures last. The client excludes `GLOBAL`, `COMPATIBLE`, `PROXY`, `PASS`, `PASS-RULE`, `REJECT`, and `REJECT-DROP` before issuing checks. `DIRECT` and custom groups remain eligible.

The optional public IP lookup contacts `https://ipwho.is/` only on request. It follows the process's outbound routing and proxy environment. The result is not proof that Mihomo handled the request. Failed requests preserve the last successful result in the dashboard.

## Dashboard execution and locks

Status, ping, mode/proxy changes, and IP lookups run in a background worker. Foreground operations temporarily restore the terminal and invoke the corresponding CLI command so editors and sudo can interact normally. Editors use `$VISUAL`, then `$EDITOR`, then `sensible-editor`; arguments are parsed without a shell.

The dashboard and tray read profile data without holding the source-cache lock between actions. Ordinary mutating CLI operations use the store lock; privileged config installation uses the runtime lock. Lock guards explicitly unlock their descriptors on drop, including when a subprocess inherited a descriptor during concurrent fork/exec.

The dashboard requires interactive stdin/stdout. It inherits terminal colors and restores terminal state on normal exit, errors, and panics.

## Desktop services and terminal launch

The GTK-free tray uses `ksni` and StatusNotifier/AppIndicator. Its icon comes from `src/assets/tray.png`.

- `minohomo-tray.service` runs the tray as the desktop user.
- `minohomo-tray-config.path` watches the default profile's settings.
- `minohomo-tray-config.service` synchronizes the enabled preference after changes.

Disabling the tray leaves the lightweight path watcher enabled so a later settings edit can turn it back on. Automatic synchronization applies only to the default profile. Neither tray automation nor core installation enables the global core service at boot.

Tray mode/proxy controls use the API. Service controls use systemctl with desktop polkit authorization. An unavailable core/API does not remove the tray's service controls.

**Open dashboard** launches a terminal through an independent transient systemd user service. Disabling the tray therefore does not close the dashboard. The launcher reads `default_terminal` on each click. Without an explicit command it tries `xdg-terminal-exec`, Debian's `x-terminal-emulator`, then `$TERMINAL`. It parses quoted arguments without a shell and supplies emulator-specific execution flags.
