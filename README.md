# Minohomo

A root-only Rust CLI that imports proxy subscriptions and direct proxy links into Mihomo. It merges proxies, applies your local routing policy, validates the generated config, and reloads the existing Mihomo service.

Subscription updates are manual. No additional daemon, timer, or systemd service is required.

## Installation

Requires an installed Mihomo binary with gVisor support. Defaults target the Debian service:

- Binary: `/usr/bin/mihomo`
- Generated config: `/etc/mihomo/config.yaml`
- Reload command: `systemctl reload mihomo.service`

### Debian package

Download the `.deb` for your architecture from GitHub Releases or the **Debian packages** workflow artifacts. CI packages are built in Debian 12 containers for **amd64** and **arm64**, for use on Debian 12 and newer.

```sh
# amd64 example; use the arm64 package on ARM machines.
sudo apt install ./minohomo_*_amd64.deb
```

The package installs `/usr/bin/minohomo`, documentation, and config templates. It does not install Mihomo, create `/etc/minohomo`, modify your active config, or start services. If you previously installed `/usr/local/bin/minohomo` manually, remove or replace that copy so it does not shadow the packaged binary.

### From source

Requires Rust/Cargo and a C build toolchain:

```sh
cargo build --release --locked
sudo install -m 755 target/release/minohomo /usr/local/bin/minohomo
```

### Initial setup

```sh
sudo minohomo init
sudo minohomo add 'https://subscription.example/profile'
sudo minohomo geosite update --no-reload
sudo minohomo update
```

If Mihomo is inactive, the reload fails but the generated config and source cache remain saved. Start the existing service:

```sh
sudo systemctl start mihomo.service
```

minohomo does not install or enable system services. Help and version work without root; all application commands require root.

## Commands

```text
minohomo init
minohomo add <LINK>...
minohomo add --proxy <HTTP_OR_HTTPS_PROXY_LINK>...
minohomo list
minohomo delete <INDEX>
minohomo update [all|_|INDEX]
minohomo geosite status
minohomo geosite update [--url URL] [--no-reload]
```

| Command | Behavior |
| --- | --- |
| `init` | Creates missing config files without overwriting existing ones. Other application commands also initialize missing files. |
| `add` | Validates and saves links. Does not fetch subscriptions or reload Mihomo. HTTP(S) URLs mean subscriptions unless `--proxy` is supplied. |
| `list` | Shows one-based indexes, links, last successful update times in UTC, and cached proxy counts. |
| `delete INDEX` | Removes a source and its cache. Run `update` to apply the removal. Indexes shift after deletion. |
| `update`, `update all`, `update _` | Refreshes all sources, generates and validates the merged config, then reloads Mihomo. |
| `update INDEX` | Refreshes one source and regenerates using cached proxies from all other sources. Never-updated sources are omitted with a message. |

Examples:

```sh
sudo minohomo add 'vless://UUID@server.example:443?security=tls&sni=server.example#MyNode'
sudo minohomo add 'socks5://user:password@server.example:1080#Upstream'
sudo minohomo add --proxy 'https://user:password@server.example:443#HTTPSProxy'
sudo minohomo list
sudo minohomo update 2
sudo minohomo delete 1
sudo minohomo update
```

A fetch, parse, or validation failure leaves the existing config and saved source state unchanged. A reload failure leaves the newly generated config and cache saved, and reports an error. Updates save the previous generated config as `config.yaml.bak`.

Deleting the last source does not stop Mihomo or replace its active config with DIRECT. Add and update another source before regeneration.

Quote links so the shell does not interpret `&` or `#`. Links, command-line arguments, shell history, and `list` output can expose credentials.

## Configuration

Edit the source files, not the generated Mihomo config:

| Path | Purpose |
| --- | --- |
| `/etc/minohomo/config.yaml` | Output path, binary, reload command, request timeout, geosite URL, proxy overrides, and base Mihomo settings |
| `/etc/minohomo/rules.yaml` | Complete routing policy and optional rule providers |
| `/etc/minohomo/sources.yaml` | Saved links, stable source IDs, timestamps, and cached proxies |
| `/etc/mihomo/config.yaml` | Generated config, read by the Mihomo daemon |
| `/etc/mihomo/GeoSite.dat` | Installed geosite database |

The config directory is mode `0700`; files created by minohomo are mode `0600`. Source caches and backups contain credentials. The Mihomo daemon must have permission to read them.

Use `--config-dir /absolute/path` to change minohomo's directory. Change `output` in its `config.yaml` to select another generated config path. The geosite database follows the output directory. Keep the daemon's data directory and config path aligned with these settings.

### Base settings and proxy overrides

```sh
sudoedit /etc/minohomo/config.yaml
```

- `mihomo` holds listener, TUN, DNS, sniffer, and proxy-group settings.
- `proxy-overrides` applies fields to every imported proxy without modifying its cached source data. The default sets `client-fingerprint: chrome`. Use `{}` to preserve source values. Overrides replace entire top-level fields; nested mappings are not merged.
- The `PROXY` selector uses `include-all: true` to include imported nodes. You can define additional groups for custom rules.
- Generation replaces `proxies`, `rules`, `rule-providers`, and `geox-url.geosite`, and removes `proxy-providers`.

Existing config files are never overwritten by `init` or a binary upgrade. See [defaults/config.yaml](defaults/config.yaml) for the current template. After editing settings, run `sudo minohomo update` to regenerate and reload. Config edits do not require a binary reinstall.

### Routing rules

```sh
sudoedit /etc/minohomo/rules.yaml
sudo minohomo update
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

The API listens on `127.0.0.1:9090`. `init` generates a random secret stored as `mihomo.secret` in `/etc/minohomo/config.yaml`. Use a compatible dashboard or the API:

```sh
curl -H 'Authorization: Bearer YOUR_SECRET' \
  http://127.0.0.1:9090/proxies/PROXY

curl -X PUT http://127.0.0.1:9090/proxies/PROXY \
  -H 'Authorization: Bearer YOUR_SECRET' \
  -H 'Content-Type: application/json' \
  -d '{"name":"EXACT_IMPORTED_NODE_NAME"}'
```

Imported names have a stable source prefix, such as `s1: MyNode`. Duplicate names receive a suffix. Deleting another source does not rename the remaining nodes.

## Geosite database

```sh
sudo minohomo geosite status
sudo minohomo geosite update
sudo minohomo geosite update --url 'https://mirror.example/geosite.dat'
```

Set a permanent source in `/etc/minohomo/config.yaml`:

```yaml
geosite-url: https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/release/geosite.dat
```

This also sets the generated config's `geox-url.geosite`. `--url` overrides it for one download only. Alternative databases must contain the categories your policy uses; the defaults require `cn`, `category-ru`, `alibaba`, and `tencent`.

`status` reports the configured URL, file path, size, modification time, category count, and domain entry count. Modification time is not the publisher's release date.

`geosite update` downloads at most 64 MiB, checks the protobuf format, and validates required rule and DNS categories with Mihomo. Invalid data leaves the installed file untouched. A successful update saves `GeoSite.dat.bak`, replaces the database atomically, and reloads Mihomo. A failed reload leaves the new database installed and reports an error.

Use `--no-reload` before the first daemon start or to defer reloading. This command does not refresh subscriptions or regenerate the main config.

Automatic geodata updates are disabled by default. Subscription updates reuse the installed database. If it is missing, Mihomo validation may download it; preload it with `geosite update --no-reload` for explicit control.

## Supported imports

Standard V2Ray subscription bodies can contain:
- Plain-text proxy links, one per line.
- Base64-encoded proxy-link lists, including URL-safe and unpadded base64.

Direct links support VLESS, VMess base64 JSON, Trojan, Shadowsocks, ShadowsocksR, Hysteria v1, Hysteria2 (`hy2`), TUIC v5, SOCKS5, HTTP, and HTTPS. Common VLESS/VMess TCP, WebSocket, gRPC, HTTP, H2, TLS, and REALITY options are supported. Shadowsocks supports legacy and SIP002 links with common obfs and v2ray plugins.

Clash YAML subscriptions, XHTTP, and arbitrary plugin formats are not supported. Proxies with `dialer-proxy` dependencies or non-proxy DIRECT/REJECT types are rejected. Mihomo performs final protocol validation.

REALITY imports preserve `support-x25519mlkem768` when supplied. Mihomo has no SpiderX field, so Xray's `spx` is not mapped to a YAML option. Fingerprint behavior can differ between cores; edit `proxy-overrides` if needed.

Subscription requests use `V2rayNG/1.8.12` as the User-Agent for compatibility with 3x-ui. HTTP downloads verify certificates, enforce timeouts, follow at most five redirects, and limit subscriptions to 16 MiB. Proxy environment variables apply when present in the root process; `sudo` may strip them.

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

Make fixes in `/etc/minohomo/config.yaml` or `rules.yaml`, then regenerate. Editing only the generated file loses changes on the next update.

## Debian builds and releases

To build a package locally, install the packaging prerequisites and `cargo-deb`, then build:

```sh
sudo apt install build-essential ca-certificates cmake dpkg-dev pkg-config
cargo install cargo-deb --version '=3.7.0' --locked
cargo deb --locked
```

Packages are written to `target/debian/`. Configuration stays under CLI control; no maintainer scripts or systemd units are included.

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

Tests require neither root nor a running daemon. Root enforcement applies to the CLI entry point.

## AI Disclosure

This program is vibecoded. I provided a lot of guidance for AI agent on how the program should function and the result is pretty much exactly what I wanted.

In theory it should be safe to use, because its only task is to store your proxies and generate /etc/mihomo/config.yaml. Nothing is running in the background aside from mihomo service that user should install as a prerequisite. However, I did NOT read any code, so no warranty.

## License

GPL-3.0-only. See [LICENSE](LICENSE).
