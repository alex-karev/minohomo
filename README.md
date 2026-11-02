# Minohomo

A Rust CLI and terminal dashboard that import proxy subscriptions and direct proxy links into Mihomo. It merges proxies, applies your local routing policy, validates the generated config, and uses sudo to install the result for the system Mihomo service.

Subscription updates are manual. Mihomo runs independently as the system service `mihomo.service`. An optional desktop tray runs separately as the systemd user service `minohomo-tray.service`.

## Installation

Requires an installed Mihomo binary with gVisor support. Defaults keep source files per-user and deploy the generated config to the system service:

- Binary: `/usr/bin/mihomo`
- Local generated config: `~/.config/minohomo/current.yaml`
- Installed config: `/etc/mihomo/config.yaml`
- Apply: `sudo minohomo apply --config <current.yaml>`, then `systemctl restart mihomo.service`

### Debian package

Download the `.deb` for your architecture from GitHub Releases or the **Debian packages** workflow artifacts. CI packages are built in Debian 12 containers for **amd64** and **arm64**, for use on Debian 12 and newer.

```sh
# amd64 example; use the arm64 package on ARM machines.
sudo apt install ./minohomo_*_amd64.deb
```

The package installs `/usr/bin/minohomo`, documentation, config templates, the tray icon, and systemd user units. It does not install Mihomo, create user config files, modify your active config, or enable/start services. If you previously installed `/usr/local/bin/minohomo` manually, remove or replace that copy so it does not shadow the packaged binary.

### From source

Requires Rust/Cargo and a C build toolchain:

```sh
cargo build --release --locked
sudo install -m 755 target/release/minohomo /usr/local/bin/minohomo
```

### Initial setup

Run minohomo as your regular user:

```sh
minohomo init
minohomo add 'https://subscription.example/profile'
minohomo geosite update --no-reload
minohomo update
```

`update` fetches sources and validates the generated config as your user. It saves `~/.config/minohomo/current.yaml`, then invokes the same minohomo executable through `sudo` with the `apply` subcommand. Expect a sudo password prompt. The helper validates a snapshot with `/usr/bin/mihomo`, installs it as `/etc/mihomo/config.yaml`, copies the local geosite database when available, and restarts `mihomo.service` using `/usr/bin/systemctl`.

Mihomo must already be installed with a system service that reads `/etc/mihomo/config.yaml` and uses `/etc/mihomo` as its data directory. TUN privileges belong to that service. minohomo itself needs neither root nor binary capabilities for its normal commands. The package depends on `sudo`; it does not install sudoers rules or enable/start services during installation.

After editing local settings or rules, regenerate from cached proxies without refreshing subscriptions:

```sh
minohomo update --no-sync
```

This still validates, installs, and restarts Mihomo. It does not fetch subscription URLs or reparse direct sources, and it preserves source timestamps and cached proxies. Sources without cached proxies contribute no nodes. At least one cached proxy is required. Validation may still download missing geodata or rule-provider data; `--no-sync` means no subscription syncing, not guaranteed offline execution.

To retry only deployment of an existing generated file:

```sh
sudo minohomo apply --config "$HOME/.config/minohomo/current.yaml" \
  --geosite "$HOME/.config/minohomo/GeoSite.dat"
```

Omit `--geosite` to preserve the system database. The helper accepts only config/database input paths; its destination, validator binary, and service name are fixed. It does not read user settings as root. Applying a user-provided Mihomo config is still a privileged operation: use normal sudo authorization, not a restricted passwordless sudoers rule that assumes this is safe for untrusted users.

## Terminal dashboard

Run `minohomo` without a subcommand to open the Ratatui dashboard:

```sh
minohomo
# Or open another profile:
minohomo --config-dir /absolute/path
```

Run it as your normal user in an interactive terminal. Existing CLI subcommands keep their behavior. When stdin or stdout is not a terminal, the dashboard exits with a message instead of emitting terminal control codes.

The layout has three panels:

- **Sources:** saved subscriptions and independent proxies, cached node counts, last update times. Add a URL/link, optionally name it, remove a selected source, or update one/all sources. Source links and credentials stay hidden outside the add dialog.
- **Network:** proxy delay tests, sorted fastest first, plus an on-demand public IP and approximate location lookup.
- **Actions:** every operation in one browsable list, including cached config generation, geosite download with/without reload, editor shortcuts, tray preference, and system service controls.

The header shows the system Mihomo service state, running mode, selected proxy, saved tray preference, geosite file size, and config/rules editor shortcuts. It refreshes every few seconds. An unavailable API does not prevent source management, editing, or service control.

| Key | Action |
| --- | --- |
| `Tab`, `Shift-Tab`, `1`/`2`/`3` | Switch panels |
| `Up`/`Down`, `j`/`k`, `PgUp`/`PgDn`, `Home`/`End` | Navigate the active list |
| `Enter` in Actions | Run selected action |
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

Editors use `$VISUAL`, then `$EDITOR`, then Debian's `sensible-editor`. Editor arguments and quoted paths work without a shell. For GUI editors, configure a wait flag, for example `EDITOR='code --wait'`, so the dashboard resumes after editing. Closing the editor returns directly to the dashboard. Apply cached sources from Actions to deploy config/rule edits without fetching subscriptions.

Status, ping, mode/proxy changes, and IP lookups run in a background worker. While a network action runs, you can browse panels or quit. Updates, geosite downloads, tray changes, and service commands temporarily restore the normal terminal, run the existing CLI, and show its output. Sudo can prompt there. Press Enter afterward to return. The dashboard does not hold the source-cache lock between actions, so another CLI process can work on the same profile.

Public IP lookup contacts **https://ipwho.is/** only when requested. The provider sees the request's outbound IP. The result includes the IP, approximate location, ISP, and lookup time. It follows system routing, TUN, and proxy environment settings; it does not prove that Mihomo carried the request. Failed/rate-limited requests show an error without discarding the last successful result. No lookup runs automatically at startup.

The dashboard inherits the terminal's foreground/background colors. Selection uses reverse video; headings use bold/dim text. No hardcoded light/dark palette or color-detection request. Minimum size is 64 columns by 22 rows. The UI restores the terminal on normal exit, errors, and panics.

## Commands

```text
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
minohomo delete <NAME>
minohomo rename <NAME> [NEW_NAME]
minohomo update [NAME] [--no-sync]
minohomo geosite status
minohomo geosite update [--url URL] [--no-reload]
sudo minohomo apply --config PATH [--geosite PATH]
```

| Command | Behavior |
| --- | --- |
| `init` | Creates missing config files without overwriting existing ones. Other application commands also initialize missing files. |
| `add` | Validates and saves links. Does not fetch subscriptions or reload Mihomo. HTTP(S) URLs mean subscriptions unless `--proxy` is supplied. |
| `list` | Shows unique source names, links, last successful update times in UTC, and cached proxy counts. |
| `list --full` | Adds indented cached node display names below each subscription. Does not expand independent proxies or fetch anything. |
| `ping` | Checks delays for every entry returned by Mihomo's `/proxies` API. Prints successful checks fastest first, then failed checks. |
| `mode [rule\|global\|direct]` | With no argument, shows the current mode from the running Mihomo API. With an argument, changes it. |
| `dump` | Prints the generated `current.yaml` verbatim, without generating or applying anything. |
| `controller` | Prints the controller URL, then `secret: SECRET` on the next line. Does not contact Mihomo. |
| `switch [PROXYNAME]` | Selects a proxy in the `PROXY` group. With no argument, shows the current selection and available names. |
| `tray run`, `tray sync`, `tray enable`, `tray disable` | Run the desktop tray, synchronize its user service, or save and apply its enabled preference. |
| `start`, `stop`, `restart` | Uses a sudo subcommand to start, stop, or restart the system `mihomo.service`. Does not generate or install a config. |
| `rename NAME [NEW_NAME]` | Renames the saved source without changing its cache or timestamps. Prompts for the new name when omitted. |
| `delete NAME` | Removes the named source and its cache. Run `update --no-sync` to apply the removal. |
| `update` | Refreshes all sources, generates and validates the merged config, then installs it through sudo and restarts Mihomo. |
| `update NAME` | Refreshes one source and regenerates using cached proxies from all other sources. Never-updated sources are omitted with a message. |
| `update --no-sync` | Regenerates from cached proxies, validates, installs, and restarts without fetching sources. Cannot be combined with a source name. |
| `apply --config PATH [--geosite PATH]` | Root-only installer used by `update`. Validates, installs to `/etc/mihomo`, and restarts the system service. |

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

## Configuration

Edit the source files, not the generated Mihomo config:

| Path | Purpose |
| --- | --- |
| `~/.config/minohomo/config.yaml` | Output path, binary, reload command, request timeout, geosite URL, proxy overrides, and base Mihomo settings |
| `~/.config/minohomo/rules.yaml` | Complete routing policy and optional rule providers |
| `~/.config/minohomo/sources.yaml` | Saved links, unique source names, internal IDs, timestamps, and cached proxies |
| `~/.config/minohomo/current.yaml` | Local generated config, passed to the privileged installer |
| `/etc/mihomo/config.yaml` | Installed config, read by the system Mihomo daemon |
| `~/.config/minohomo/GeoSite.dat` | Local geosite database, copied by the privileged installer |
| `/etc/mihomo/GeoSite.dat` | Installed system geosite database |

The config directory is mode `0700`; files created by minohomo are mode `0600`. Source caches and backups contain credentials. The Mihomo daemon must have permission to read them.

The default directory is `$HOME/.config/minohomo`. Use `--config-dir /absolute/path` to change it. `output: current.yaml` resolves relative to that directory; absolute local output paths are also supported. The local geosite database follows the output directory. The built-in installer always targets `/etc/mihomo/config.yaml` and `mihomo.service`; use a custom `reload-command` for a different deployment. `mihomo-binary` controls local validation only; privileged validation always uses `/usr/bin/mihomo`.

### Base settings and proxy overrides

```sh
editor ~/.config/minohomo/config.yaml
```

- `mihomo` holds listener, TUN, DNS, sniffer, and proxy-group settings.
- `proxy-overrides` applies fields to every imported proxy without modifying its cached source data. The default sets `client-fingerprint: chrome`. Use `{}` to preserve source values. Overrides replace entire top-level fields; nested mappings are not merged.
- The `PROXY` selector uses `include-all: true` to include imported nodes. You can define additional groups for custom rules.
- Generation replaces `proxies`, `rules`, `rule-providers`, and `geox-url.geosite`, and removes `proxy-providers`.

Existing config files are never overwritten by `init` or a binary upgrade. See [defaults/config.yaml](defaults/config.yaml) for the current template. After editing settings, run `minohomo update` to regenerate and apply. Config edits do not require a binary reinstall.

### Routing rules

```sh
editor ~/.config/minohomo/rules.yaml
minohomo update
```

Example structure:

```yaml
rule-providers: {}
rules:
  - DOMAIN-SUFFIX,example.org,DIRECT
  - MATCH,PROXY
```

Rules run in order. minohomo does not restore removed rules or append a hidden catch-all. Subscription rules, groups, providers, DNS, listeners, and TUN settings are ignored.

Keep TUN's `route-exclude-address` aligned with your local DIRECT rules. Excluded ranges bypass TUN entirely; DIRECT rules also cover requests submitted through the local proxy listener.

You can add `rule-providers` to `rules.yaml`. HTTP providers with refresh intervals may update automatically inside Mihomo. The defaults contain no proxy or rule providers.

## Default network behavior

- TUN uses interface `mihomo`, the `gvisor` userspace stack, and auto-redirect disabled.
- Local, private, loopback, link-local, multicast, and Tailscale IPv4/IPv6 ranges follow normal system routes.
- Local domains, Chinese and Russian domain suffixes and geosite categories, Alibaba, and Tencent use DIRECT.
- Unmatched traffic uses the selected node in `PROXY`.
- HTTP, TLS, and QUIC sniffing recovers hostnames for routing and overrides destination IPs. This lets proxied connections use the hostname instead of an incorrect address returned by local DNS.
- Public DNS uses literal-IP AliDNS DoH with certificate verification. MagicDNS queries use `100.100.100.100#tailscale0`; Tailscale must be running to resolve those names.
- Fake-IP exclusions use suffix patterns such as `+.ts.net`, which include nested names like `host.tailnet.ts.net`.

Domain lists cannot identify every organization. Sniffing cannot recover every encrypted hostname, including ECH-hidden SNI. This is not a strict DNS-leak-prevention configuration.

The mixed listener serves **SOCKS5 and HTTP on `127.0.0.1:7897`**. It is not exposed to the LAN:

```sh
curl --proxy socks5h://127.0.0.1:7897 https://example.com
```

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

`ping` uses Mihomo's HTTP delay check, not ICMP. It requests `https://www.gstatic.com/generate_204` with a five-second timeout per entry and up to eight checks in parallel. It uses API names without matching them to local sources. All entries returned by Mihomo are checked, including groups and built-in entries. Failed checks appear last.

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

These commands invoke the same executable through sudo, then run `/usr/bin/systemctl start|stop|restart mihomo.service`. They do not read root's minohomo config, enable the service at boot, or install a generated file. Run `update` first to deploy a config. They are independent of a custom `reload-command`.

### Desktop tray

The tray uses `src/assets/tray.png` and the desktop's StatusNotifier/AppIndicator support. KDE and compatible panels support it directly. GNOME needs an AppIndicator extension. No GTK runtime is required.

```yaml
# ~/.config/minohomo/config.yaml, top level
tray-enabled: true
```

The preference defaults to true, including for existing config files. After installing the package, run `minohomo init` as your desktop user. Normal CLI commands in a desktop session automatically synchronize the tray service. To request synchronization explicitly:

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
- Enable, disable, or restart the system `mihomo.service`.
- Open the terminal dashboard in your preferred terminal emulator.

The **Open dashboard** menu item launches the same profile's TUI in a new terminal. Set the top-level terminal preference in `config.yaml`:

```yaml
default_terminal: null             # System default
# default_terminal: "kitty"
# default_terminal: "gnome-terminal"
# default_terminal: "konsole --separate"
# default_terminal: "alacritty"
```

Unset, null, or empty uses `xdg-terminal-exec` when available, then Debian's `x-terminal-emulator` alternative, then `$TERMINAL` as a fallback. Custom commands support quoted paths and arguments without a shell. The launcher supplies the emulator's execution flag; a custom command can include an explicit trailing `--`, `-e`, `-x`, or `--execute` flag to override it. The tray reads this setting on each click. `default-terminal` is also accepted as an alias.

The terminal starts in its own transient systemd user service, so disabling the tray does not close the dashboard. Launch failures appear in the tray's error entry; errors after terminal startup appear in the user journal. This requires a working systemd user manager and an installed terminal emulator.

Enable/disable in this menu means start/stop, not boot-time enablement. Service actions use systemctl and the desktop polkit authentication agent. Install and enable a polkit agent for your desktop; unauthorized or cancelled actions show an error in the menu and user journal. Mode and proxy changes use the authenticated Mihomo API without root access. The tray remains available when Mihomo is stopped or the API is unreachable. Runtime changes do not rewrite generated config.

The tray does not hold the source-cache lock, so normal CLI updates can run alongside it. Check service errors with:

```sh
journalctl --user -u minohomo-tray.service -u minohomo-tray-config.service
```

Automatic service management uses the default profile only. To run another profile in the foreground, use `minohomo --config-dir /absolute/path tray run`. Source-only installs can run this command directly. To use the supplied user units with a source build, install the binary at `/usr/bin/minohomo`, copy `packaging/minohomo-tray*` to `~/.config/systemd/user/`, run `systemctl --user daemon-reload`, then `minohomo tray sync`.

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

Source names must be unique. `--name` accepts one link per command; bulk adds without `--name` assign a separate random name to each source. Existing sources without names receive persistent random names on their next use. For legacy duplicate names, the first source keeps the name and later sources receive random names. minohomo prints these migration assignments.

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

Targets are always exact, case-sensitive source names. Numeric indexes are no longer supported. Quote names containing spaces:

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

## Supported imports

Standard V2Ray subscription bodies can contain:
- Plain-text proxy links, one per line.
- Base64-encoded proxy-link lists, including URL-safe and unpadded base64.

Direct links support VLESS, VMess base64 JSON, Trojan, Shadowsocks, ShadowsocksR, Hysteria v1, Hysteria2 (`hy2`), TUIC v5, SOCKS5, HTTP, and HTTPS. Common VLESS/VMess TCP, WebSocket, gRPC, HTTP, H2, TLS, and REALITY options are supported. Shadowsocks supports legacy and SIP002 links with common obfs and v2ray plugins.

Clash YAML subscriptions, XHTTP, and arbitrary plugin formats are not supported. Proxies with `dialer-proxy` dependencies or non-proxy DIRECT/REJECT types are rejected. Mihomo performs final protocol validation.

REALITY imports preserve `support-x25519mlkem768` when supplied. Mihomo has no SpiderX field, so Xray's `spx` is not mapped to a YAML option. Fingerprint behavior can differ between cores; edit `proxy-overrides` if needed.

Subscription requests use `V2rayNG/1.8.12` as the User-Agent for compatibility with 3x-ui. HTTP downloads verify certificates, enforce timeouts, follow at most five redirects, and limit subscriptions to 16 MiB. Proxy environment variables apply when present in the current process.

## Troubleshooting

Test the explicit proxy and transparent TUN paths separately:

```sh
# Explicit proxy
curl --noproxy '' --proxy http://127.0.0.1:7897 --max-time 20 -I https://google.com

# Transparent routing, including redirects
curl --noproxy '*' --max-time 20 -IL https://google.com

# Service logs and routes
sudo journalctl -u mihomo.service -b -n 150 --no-pager
ip -brief address
ip rule
ip route show table all
```

- **Proxy works, TUN times out:** check `tun.enable`, use `stack: gvisor`, and inspect routes and logs. An interface existing does not prove traffic reaches the outbound handler.
- **TUN connects to some sites but hangs on TLS elsewhere:** check sniffing and destination override. Set `override-destination: true` globally and for HTTP, TLS, and QUIC. Per-protocol values override the global value.
- **DIRECT reports a DNS error:** check resolver reachability and bootstrap dependencies. Literal-IP DoH avoids resolving a resolver hostname through an unavailable or intercepted OS DNS path.
- **Tailscale names fail:** check that Tailscale is running, the resolver uses `#tailscale0`, and the fake-IP filter includes `+.ts.net`.
- **REALITY authentication fails:** verify the public key, short ID, SNI, fingerprint, and ML-KEM flag against the same source link.
- **Listener fails to bind:** stop other clients using the same port or change `mihomo.mixed-port`.

Make fixes in `~/.config/minohomo/config.yaml` or `rules.yaml`, then regenerate. Editing only the generated file loses changes on the next update.

## Debian builds and releases

To build a package locally, install the packaging prerequisites and `cargo-deb`, then build:

```sh
sudo apt install build-essential ca-certificates cmake dpkg-dev pkg-config
cargo install cargo-deb --version '=3.7.0' --locked
cargo deb --locked
```

Packages are written to `target/debian/`. The package includes tray user units, not a system Mihomo unit. No maintainer scripts activate services during package installation.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

The optional real-database test uses the installed `/usr/bin/mihomo` and writes only to a temporary directory:

```sh
MINOHOMO_TEST_GEOSITE=/path/to/GeoSite.dat \
  cargo test --locked real_database_passes_mihomo_validation_and_replacement -- --ignored --nocapture
```

Tests require neither root nor a running daemon.

## AI Disclosure

This program is vibecoded. I provided a lot of guidance for AI agent on how the program should function and the result is pretty much exactly what I wanted.

In theory it should be safe to use, because its only task is to store your proxies and generate ~/.config/minohomo/current.yaml. The optional tray and its config watcher run in the background alongside the system Mihomo service that the user must install as a prerequisite. However, I did NOT read any code, so no warranty.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
