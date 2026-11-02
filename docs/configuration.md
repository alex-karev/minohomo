# Configuration

[Back to README](../README.md) · [Usage](usage.md) · [Implementation](implementation.md)

## Files and settings

Edit the source files, not the generated Mihomo config:

| Path | Purpose |
| --- | --- |
| `~/.config/minohomo/config.yaml` | Output path, binary, reload command, request timeout, geosite URL, proxy overrides, and base Mihomo settings |
| `~/.config/minohomo/rules.yaml` | Complete routing policy and optional rule providers |
| `~/.config/minohomo/sources.yaml` | Saved links, unique source names, internal IDs, timestamps, and cached proxies |
| `~/.config/minohomo/current.yaml` | Local generated config, passed to the privileged installer |
| `/var/lib/minohomo/mihomo` | Managed runtime binary, downloaded and managed by Minohomo |
| `/var/lib/minohomo/config.yaml` | Installed config, read by `minohomo.service` |
| `~/.config/minohomo/GeoSite.dat` | Local geosite database, copied by the privileged installer |
| `/var/lib/minohomo/GeoSite.dat` | Installed system geosite database |

The config directory is mode `0700`; files created by minohomo are mode `0600`. Source caches and backups contain credentials. The Mihomo daemon must have permission to read them.

The default directory is `$HOME/.config/minohomo`. Use `--config-dir /absolute/path` to change it. `output: current.yaml` resolves relative to that directory; absolute local output paths are also supported. The local geosite database follows the output directory. The built-in installer always targets `/var/lib/minohomo/config.yaml` and `minohomo.service`; use a custom `reload-command` for a different deployment. The managed core lives at `/var/lib/minohomo/mihomo`. A different `mihomo-binary` overrides local validation for custom `reload-command` deployments; privileged validation always uses the managed core.

### Base settings and proxy overrides

```sh
editor ~/.config/minohomo/config.yaml
```

- `mihomo` holds listener, TUN, DNS, sniffer, and proxy-group settings.
- `proxy-overrides` applies fields to every imported proxy without modifying its cached source data. The default sets `client-fingerprint: chrome`. Use `{}` to preserve source values. Overrides replace entire top-level fields; nested mappings are not merged.
- The `PROXY` selector uses `include-all: true` to include imported nodes. You can define additional groups for custom rules.
- Generation replaces `proxies`, `rules`, `rule-providers`, and `geox-url.geosite`, and removes `proxy-providers`.

`init` and application upgrades do not overwrite your settings. See [defaults/config.yaml](../defaults/config.yaml) for the current template. After editing settings, run `minohomo update --no-sync` to apply them using cached nodes. Config edits do not require a binary reinstall.

### Routing rules

```sh
editor ~/.config/minohomo/rules.yaml
minohomo update --no-sync
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

## Supported imports

Standard V2Ray subscription bodies can contain:
- Plain-text proxy links, one per line.
- Base64-encoded proxy-link lists, including URL-safe and unpadded base64.

Direct links support VLESS, VMess base64 JSON, Trojan, Shadowsocks, ShadowsocksR, Hysteria v1, Hysteria2 (`hy2`), TUIC v5, SOCKS5, HTTP, and HTTPS. Common VLESS/VMess TCP, WebSocket, gRPC, HTTP, H2, TLS, and REALITY options are supported. Shadowsocks supports legacy and SIP002 links with common obfs and v2ray plugins.

Clash YAML subscriptions, XHTTP, and arbitrary plugin formats are not supported. Proxies with `dialer-proxy` dependencies or non-proxy DIRECT/REJECT types are rejected. Mihomo performs final protocol validation.

REALITY imports preserve `support-x25519mlkem768` when supplied. Mihomo has no SpiderX field, so Xray's `spx` is not mapped to a YAML option. Fingerprint behavior can differ between cores; edit `proxy-overrides` if needed.

Subscription requests use `V2rayNG/1.8.12` as the User-Agent for compatibility with 3x-ui. HTTP downloads verify certificates, enforce timeouts, follow at most five redirects, and limit subscriptions to 16 MiB. Proxy environment variables apply when present in the current process.

## Tray and terminal preferences

```yaml
tray-enabled: true
default_terminal: null
# default_terminal: "kitty --single-instance"
# default_terminal: "gnome-terminal"
# default_terminal: "konsole --separate"
```

The tray reloads the terminal preference on each **Open dashboard** click. Null or empty selects the system default. See [desktop tray usage](usage.md#desktop-tray) for terminal detection and service controls.

## Download mirrors

```yaml
core-version-url: https://github.com/MetaCubeX/mihomo/releases/latest/download/version.txt
core-download-url: https://github.com/MetaCubeX/mihomo/releases/download/{version}/{artifact}-{version}.{ext}
geosite-url: https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/release/geosite.dat
```

The version endpoint must return a tag such as `v1.19.33`. Archive templates support `{version}`, `{artifact}`, and `{ext}`. A custom version endpoint still uses GitHub archives unless you also change `core-download-url`. Core downloads require HTTPS, except HTTP on loopback for local testing. These endpoints supply executable code; trust the source.

For a one-off core update, pass `--version-url URL` and `--download-url TEMPLATE`. For a one-off geosite download, pass `--url URL`. Neither changes the saved settings.
