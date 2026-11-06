# Usage

[Back to README](../README.md) · [Configuration](configuration.md) · [Troubleshooting](troubleshooting.md)

## Terminal dashboard

Run `minohomo` without a subcommand to open the Ratatui dashboard:

```sh
minohomo
# Or open another profile:
minohomo --config-dir /absolute/path
```

The **Minohomo** application-menu shortcut opens a new terminal using the same launcher as the tray. Run `minohomo --window` to do this from the CLI. Both respect `default-terminal` in `config.yaml`; the launcher works without the tray running.

Run it as your normal user in an interactive terminal. When stdin or stdout is not a terminal, the dashboard exits with a message instead of emitting terminal control codes.

The layout has four panels:

- **Sources:** saved subscriptions and independent proxies, cached node counts, last update times. Add a URL/link, optionally name it, remove a selected source, or update one/all sources. Source links and credentials stay hidden outside the add dialog.
- **Network:** proxy delay tests, sorted fastest first, plus an on-demand public IP and approximate location lookup.
- **Connections (3):** automatically updated connections, newest first, with green **Open** and red **Closed** indicators. Shows destination, transport/inbound type, chain, download/upload totals, and speeds on wider terminals. Enter opens scrollable details including rule, DNS mode, sniffed host, source/destination addresses, inbound fields, and process data when Mihomo supplies them.
- **Actions (4):** every operation in one browsable list, including applying cached sources, core updates, geosite download with/without reload, editor shortcuts, tray preference, and system service controls.

The header shows the system Mihomo service state, running mode, selected proxy, installed core version, saved tray preference, geosite file size, and config/rules editor shortcuts. It refreshes every few seconds. An unavailable API does not prevent source management, editing, or service control.

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab`, `1`/`2`/`3`/`4` | Switch panels |
| `Up`/`Down`, `j`/`k`, `PgUp`/`PgDn`, `Home`/`End` | Navigate the active list |
| `Enter` in Actions | Run selected action |
| `Enter` in Connections | Open selected connection details; Enter or Esc closes them |
| `Ctrl-X` in Connections or connection details | Copy connection details through terminal clipboard support (OSC 52) |
| `a`, `d` | Add or remove a source |
| `n` in Sources | Rename the selected source |
| `u`, `U` | Update selected source or all sources |
| `g` | Update geosite and reload Mihomo |
| `c`, `r` | Edit config.yaml or rules.yaml |
| `m`, `x` | Pick running mode or proxy; type to filter choices |
| `s` | Start, stop, or restart Mihomo |
| `p`, `i` | Ping proxies or look up public IP/location |
| `t` | Toggle tray-enabled and synchronize its user service |
| `F5` | Refresh status |
| `?`, `F1` | Keyboard help; arrows scroll it |
| `Esc` | Cancel a dialog, or quit from the main screen |
| `q`, `Ctrl-C` | Quit from the main screen; Ctrl-C also exits from dialogs |

In the add form, `Tab` moves between the name, link, and HTTP(S)-proxy checkbox. Paste a subscription URL or direct proxy link. Use the checkbox only when an HTTP(S) link describes a proxy endpoint rather than a subscription. `Space` toggles it, `Ctrl-U` clears a text field, and `Enter` saves. Deletion, stop, and restart ask for confirmation; only `y` confirms, while `Enter` cancels.

Editors use `default-editor` from config.yaml when nonempty, otherwise `$VISUAL`, then `$EDITOR`, then Debian's `sensible-editor`. Editor arguments and quoted paths work without a shell. For GUI editors, configure a wait flag, for example `EDITOR='code --wait'`, so the dashboard resumes after editing. Closing the editor returns directly to the dashboard. Apply cached sources from Actions to deploy config/rule edits without fetching subscriptions.

Status, ping, mode/proxy changes, and IP lookups run in a background worker. While a network action runs, you can browse panels or quit. Updates, geosite downloads, tray changes, and service commands temporarily restore the normal terminal, run the existing CLI, and show its output. Sudo can prompt there. Press Enter afterward to return. The dashboard does not hold the source-cache lock between actions, so another CLI process can work on the same profile.

Public IP lookup contacts **https://ipwho.is/** only when requested. The provider sees the request's outbound IP. The result includes the IP, approximate location, ISP, and lookup time. It follows system routing, TUN, and proxy environment settings; it does not prove that Mihomo carried the request. Failed/rate-limited requests show an error without discarding the last successful result. No lookup runs automatically at startup.

Connections poll the authenticated Mihomo API in a separate background worker, roughly once per second plus request time. Updates continue during ping and IP lookups and while viewing details. Selection follows connection IDs as new entries arrive. History exists only in memory for this dashboard session; all observed open connections and up to 1000 recently started closed connections are retained. Mihomo does not provide past closed connections through this endpoint. Connections that open and close between polls can be missed. Closed totals are the last observed values, not guaranteed final totals; speeds are calculated from counter differences between polls. API failures leave the last known list/status intact and show a stale warning rather than falsely marking connections closed.

Ctrl-X sends details using OSC 52. Clipboard access must be supported and permitted by the terminal and any terminal multiplexer. The application cannot confirm that the terminal accepted the copy. Connection details include browsing destinations, addresses, process paths, and inbound usernames; review them before sharing.

The dashboard inherits the terminal's foreground/background colors, except for green/red connection status indicators. Selection uses reverse video; headings use bold/dim text. No hardcoded light/dark background palette or color-detection request. Minimum size is 64 columns by 22 rows. The UI restores the terminal on normal exit, errors, and panics.

## Commands

```text
minohomo --window
minohomo init
minohomo add <LINK>... [--name NAME]
minohomo add --proxy <HTTP_OR_HTTPS_PROXY_LINK>... [--name NAME]
minohomo list [--full]
minohomo ping
minohomo mode [rule|global|direct]
minohomo dump
minohomo controller
minohomo switch [PROXYNAME]
minohomo start
minohomo stop
minohomo restart
minohomo core version
minohomo core update [--version-url URL] [--download-url TEMPLATE]
minohomo tray run|sync|enable|disable
minohomo delete <NAME>
minohomo rename <NAME> [NEW_NAME]
minohomo update [NAME] [--no-sync]
minohomo geosite status
minohomo geosite update [--url URL] [--no-reload]
sudo minohomo apply --config PATH [--geosite PATH]
```

| Command | Behavior |
| --- | --- |
| `--window` | Opens the dashboard in a new terminal using `default-terminal` or the system default. Used by the application-menu shortcut. |
| `init` | Creates missing config files without overwriting existing ones. Other application commands also initialize missing files. |
| `add` | Validates and saves links. Does not fetch subscriptions or reload Mihomo. HTTP(S) URLs mean subscriptions unless `--proxy` is supplied. |
| `list` | Shows unique source names, links, last successful update times in UTC, and cached proxy counts. |
| `list --full` | Adds indented cached node display names below each subscription. Does not expand independent proxies or fetch anything. |
| `ping` | Checks delays for API entries other than `GLOBAL`, `COMPATIBLE`, `PROXY`, `PASS`, `PASS-RULE`, `REJECT`, and `REJECT-DROP`. Prints successful checks fastest first, then failed checks. |
| `mode [rule\|global\|direct]` | With no argument, shows the current mode from the running Mihomo API. With an argument, changes it. |
| `dump` | Prints the generated `current.yaml` verbatim, without generating or applying anything. |
| `controller` | Prints the controller URL, then `secret: SECRET` on the next line. Does not contact Mihomo. |
| `switch [PROXYNAME]` | Selects a proxy in the `PROXY` group. With no argument, shows the current selection and available names. |
| `tray run`, `tray sync`, `tray enable`, `tray disable` | Run the desktop tray, synchronize its user service, or save and apply its enabled preference. |
| `start`, `stop`, `restart` | Uses a sudo subcommand to start, stop, or restart the system `minohomo.service`. Does not generate or install a config. |
| `rename NAME [NEW_NAME]` | Renames the saved source without changing its cache or timestamps. Prompts for the new name when omitted. |
| `delete NAME` | Removes the named source and its cache. Run `update --no-sync` to apply the removal. |
| `update` | Refreshes all sources, generates and validates the merged config, then installs it through sudo and restarts Mihomo. |
| `update NAME` | Refreshes one source and regenerates using cached proxies from all other sources. Never-updated sources are omitted with a message. |
| `update --no-sync` | Regenerates from cached proxies, validates, installs, and restarts without fetching sources. Cannot be combined with a source name. |
| `core version` | Prints the managed core version, or `Not installed` when absent. |
| `core update [--version-url URL] [--download-url TEMPLATE]` | Installs the latest core. Restarts an already-running service. |
| `apply --config PATH [--geosite PATH]` | Root-only installer used by `update`. Copies a missing runtime core from the package bundle, or downloads one if both local copies are missing, validates, installs to `/var/lib/minohomo`, and restarts `minohomo.service`. |

Examples:

```sh
minohomo add 'vless://UUID@server.example:443?security=tls&sni=server.example#MyNode'
minohomo add 'socks5://user:password@server.example:1080#Upstream' --name Upstream
minohomo add --proxy 'https://user:password@server.example:443#HTTPSProxy'
minohomo list
minohomo update Upstream
minohomo delete Upstream
minohomo update
```

A fetch, parse, or local validation failure leaves the local generated config and saved source state unchanged. A sudo, install, system validation, or restart failure leaves the newly generated local config and cache saved so deployment can be retried. System validation failure leaves the installed config and database unchanged. Updates save the previous local config as `current.yaml.bak`; the helper saves system config/database backups before replacement. A restart failure leaves the new system config installed and reports an error.

Deleting the last source does not stop Mihomo or replace its active config with DIRECT. Add and update another source before regeneration.

Quote links so the shell does not interpret `&` or `#`. Links, command-line arguments, shell history, and `list` output can expose credentials.

### Select a proxy

Mihomo remembers the selected node. Without a saved selection, the selector uses its default choice. It does not automatically fail over.

The API listens on `127.0.0.1:9090`. `init` generates a random secret stored as `mihomo.secret` in `~/.config/minohomo/config.yaml`. Runtime commands use `external-controller` and `secret` from the last generated `current.yaml`, falling back to the base settings if it does not exist. Controller requests bypass proxy environment variables. The running daemon must use the same config and secret. `minohomo controller` prints that URL and secret without requiring the service to be running:

```text
http://127.0.0.1:9090
secret: YOUR_SECRET
```

`minohomo dump` prints the generated config file exactly as saved (or the configured `output` path). It reports an error if no generated config exists. Both commands can expose credentials; avoid sending their output to public logs.

```sh
minohomo ping
minohomo mode
minohomo mode direct
minohomo mode rule
minohomo switch Personal
minohomo switch 'FooBar: Proxy1'
minohomo switch
```

`ping` uses Mihomo's HTTP delay check, not ICMP. It requests `https://www.gstatic.com/generate_204` with a five-second timeout per entry and up to eight checks in parallel. It uses API names without matching them to local sources. Checks exclude `GLOBAL`, `COMPATIBLE`, `PROXY`, `PASS`, `PASS-RULE`, `REJECT`, and `REJECT-DROP`. Other API entries, including `DIRECT` and custom groups, remain eligible. Failed checks appear last.

`switch` accepts an independent proxy's saved source name, a full generated node name, or a unique cached node display name from `list --full`. It uses the same name-generation logic as `current.yaml`, including subscription prefixes and duplicate suffixes. A subscription source name works only when it contains one cached node. If a name matches several nodes, the error lists the full names to use. The selected node must also exist in the running `PROXY` group; apply updated caches before switching.

`mode` and `switch` do not rewrite local settings, refresh subscriptions, or restart Mihomo. Mode changes last until changed again or the config is reloaded. To change the mode after future restarts, edit `mihomo.mode` and regenerate. Proxy selection persistence follows Mihomo's `profile.store-selected` setting.

### Expanded subscription listing

```sh
minohomo list --full
```

The normal listing stays unchanged; subscription node names appear below their entry with extra indentation:

```text
FooBar [subscription] https://subscription.example/profile
   Last update: 2026-11-01T00:00:00Z | Cached proxies: 2
      Proxy1
      Proxy2
Personal [proxy] trojan://PASSWORD@server.example:443#Proxy1
   Last update: 2026-11-01T00:00:00Z | Cached proxies: 1
```

### Service control

```sh
minohomo start
minohomo stop
minohomo restart
```

For Debian/Arch builds, these commands first copy the bundled core into `/var/lib/minohomo` if it is missing. If neither the runtime core nor the bundle exists, they download a core automatically. They invoke the same executable through sudo, then run `/usr/bin/systemctl start|stop|restart minohomo.service`. They do not read root's minohomo config, enable the service at boot, or install a generated file. Run `update` first to deploy a config. They are independent of a custom `reload-command`.

### Desktop tray

The tray uses `src/assets/tray.png` and the desktop's StatusNotifier/AppIndicator support. KDE and compatible panels support it directly. GNOME needs an AppIndicator extension. No GTK runtime is required.

```yaml
# ~/.config/minohomo/config.yaml, top level
tray-enabled: true
```

The preference defaults to true, when omitted. After installing the package, run `minohomo init` as your desktop user. Normal CLI commands in a desktop session automatically synchronize the tray service. To request synchronization explicitly:

```sh
minohomo tray sync
# Save the preference and synchronize in one command:
minohomo tray disable
minohomo tray enable
```

This enables and starts `minohomo-tray.service` and `minohomo-tray-config.path` for your user. The tray starts with future graphical sessions. The path unit watches `~/.config/minohomo/config.yaml`. Changing `tray-enabled` to false automatically stops and disables the tray service; changing it back to true enables and starts it again. The lightweight path watcher stays enabled while the icon is disabled so it can detect that change. Package installation itself does not enable services. Neither unit enables or disables the system Mihomo service at boot.

The tray shows Mihomo's running/stopped state, current mode, and selected proxy. It refreshes every few seconds. Its menu lets you:

- Switch between Rule, Global, and Direct modes.
- Select a proxy from the running `PROXY` group, or `GLOBAL` in Global mode.
- Show **Stop** while Mihomo is running, or **Start** while it is stopped. **Restart** is always available. These control the system `minohomo.service`, not its boot-enabled preference.
- Open the terminal dashboard in your preferred terminal emulator.

The **Open dashboard** menu item launches the same profile's TUI in a new terminal. Set the top-level terminal preference in `config.yaml`:

```yaml
default-terminal: null             # System default
# default-terminal: "kitty"
# default-terminal: "gnome-terminal"
# default-terminal: "konsole --separate"
# default-terminal: "alacritty"
```

Unset, null, or empty uses `xdg-terminal-exec` when available, then Debian's `x-terminal-emulator` alternative, then `$TERMINAL` as a fallback. Custom commands support quoted paths and arguments without a shell. The launcher supplies the emulator's execution flag; a custom command can include an explicit trailing `--`, `-e`, `-x`, or `--execute` flag to override it. The tray reads this setting on each click.

The terminal starts in its own transient systemd user service, so disabling the tray does not close the dashboard. Launch failures appear in the tray's error entry; errors after terminal startup appear in the user journal. This requires a working systemd user manager and an installed terminal emulator.

Enable/disable in this menu means start/stop, not boot-time enablement. Service actions use systemctl and the desktop polkit authentication agent. Install and enable a polkit agent for your desktop; unauthorized or cancelled actions show an error in the menu and user journal. Mode and proxy changes use the authenticated Mihomo API without root access. The tray remains available when Mihomo is stopped or the API is unreachable. Runtime changes do not rewrite generated config.

The tray does not hold the source-cache lock, so normal CLI updates can run alongside it. Check service errors with:

```sh
journalctl --user -u minohomo-tray.service -u minohomo-tray-config.service
```

Automatic service management uses the default profile only. All profiles share the system core and runtime service. To run another profile in the foreground, use `minohomo --config-dir /absolute/path tray run`. Source-only installs can run this command directly. To use the supplied user units with a source build, install the binary at `/usr/bin/minohomo`, copy `packaging/minohomo-tray*` to `~/.config/systemd/user/`, run `systemctl --user daemon-reload`, then `minohomo tray sync`.

To remove all tray automation, set `tray-enabled: false`, then run `systemctl --user disable --now minohomo-tray.service minohomo-tray-config.path`. Normal desktop CLI commands will re-enable the config watcher unless you mask it with `systemctl --user mask minohomo-tray-config.path`.

### Source names and display names

Every subscription and independent proxy has a unique **source name**, used to identify the source in `delete`, `rename`, `update`, `list`, and direct-proxy `switch` commands. Pass `--name` to choose it. Without that flag, minohomo assigns a random lowercase hex name of exactly six characters, such as `a3f09c`, and prints it after adding the source. Names persist across updates.

A node's **display name** comes from its imported proxy link or subscription entry. Generated Mihomo names follow these rules:

| Source | Source name | Node display name | Name in `current.yaml` |
| --- | --- | --- | --- |
| Independent proxy | `Personal` | `Proxy1` | `Proxy1` |
| Subscription | `FooBar` | `Proxy1` | `FooBar: Proxy1` |
| Subscription without `--name` | `a3f09c` | `Proxy1` | `a3f09c: Proxy1` |

```sh
minohomo add 'https://subscription.example/profile' --name FooBar
minohomo add 'trojan://PASSWORD@server.example:443#Proxy1' --name Personal
minohomo update FooBar
minohomo delete Personal
```

`--name` changes the source name, not the independent proxy's display name. Edit the link's `#fragment` when adding a direct proxy to choose its display name. Duplicate generated display names receive a suffix to keep the Mihomo config valid.

Source names must be unique. `--name` accepts one link per command; bulk adds without `--name` assign a separate random name to each source. 

### Rename a source

Use the same exact saved source name as `delete`, including numeric names:

```sh
minohomo rename Personal "Work proxy"
minohomo rename a3f09c Office
# Omit the new name to enter it at a prompt:
minohomo rename Office
# Names beginning with a dash go after --:
minohomo rename -- --old --new
```

The new name must be nonempty, contain no control characters, and be unique among saved sources. The prompt accepts spaces and Unicode. Empty input, EOF, an unknown source, or a duplicate name leaves the saved source unchanged. Renaming to its current name is a no-op.

In the TUI, select a source in **Sources**, then press `n`. The dialog starts with its current name. Edit it, or use `Ctrl-U` to clear it; `Enter` saves and `Esc` cancels. Rename is also listed under Actions.

Renaming changes only the saved source name. It preserves the source ID, URL/link, cached proxy data, and timestamps. It does not fetch subscriptions, edit the generated config, or restart Mihomo. Independent proxies keep their node display names. Subscription names become generated node prefixes on the next generation, so run `minohomo update --no-sync` or choose **Apply cached sources** in Actions to deploy those changes.

### Select sources by name

Targets are always exact, case-sensitive source names. Use saved names, not numeric row indexes. Quote names containing spaces:

```sh
minohomo update 'Work VPN'
minohomo delete a3f09c
minohomo list
```

Run `minohomo update` without a target to refresh all sources. Explicit `all`, `_`, and numeric strings are ordinary names, not special selectors. For example, `minohomo update 123456` updates the source named `123456`. `update --no-sync` regenerates the whole config and cannot take a source name.

## Geosite database

```sh
minohomo geosite status
minohomo geosite update
minohomo geosite update --url 'https://mirror.example/geosite.dat'
```

Set a permanent source in `~/.config/minohomo/config.yaml`:

```yaml
geosite-url: https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/release/geosite.dat
```

This also sets the generated config's `geox-url.geosite`. `--url` overrides it for one download only. Alternative databases must contain the categories your policy uses; the defaults require `cn`, `category-ru`, `alibaba`, and `tencent`.

`status` reports the configured URL, file path, size, modification time, category count, and domain entry count. Modification time is not the publisher's release date.

`geosite update` downloads at most 64 MiB, checks the protobuf format, and validates required rule and DNS categories with Mihomo. Invalid data leaves the installed file untouched. A successful update saves `GeoSite.dat.bak`, replaces the database atomically, and applies the existing local `current.yaml` plus the new database through sudo, restarting the system service. Run `minohomo update` first if no local config exists, or use `--no-reload` and deploy with the next update. A failed apply leaves the local database saved for retry.

Use `--no-reload` before the first daemon start or to defer reloading. This command does not refresh subscriptions or regenerate the main config.

Automatic geodata updates are disabled by default. Subscription updates reuse the installed database. If it is missing, Mihomo validation may download it; preload it with `geosite update --no-reload` for explicit control.
