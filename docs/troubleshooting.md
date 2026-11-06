# Troubleshooting

[Back to README](../README.md) · [Usage](usage.md) · [Configuration](configuration.md)

## Network and routing

Test the explicit proxy and transparent TUN paths separately:

```sh
# Explicit proxy
curl --noproxy '' --proxy http://127.0.0.1:7897 --max-time 20 -I https://google.com

# Transparent routing, including redirects
curl --noproxy '*' --max-time 20 -IL https://google.com

# Service logs and routes
sudo journalctl -u minohomo.service -b -n 150 --no-pager
ip -brief address
ip rule
ip route show table all
```

- **Proxy works, TUN times out:** check `tun.enable`, use `stack: gvisor`, and inspect routes and logs. An interface existing does not prove traffic reaches the outbound handler.
- **TUN connects to some sites but hangs on TLS elsewhere:** check sniffing and destination override. Set `override-destination: true` globally and for HTTP, TLS, and QUIC. Per-protocol values override the global value.
- **WeChat file transfers or other DIRECT connections stall with an IPv6 remote destination:** set both `mihomo.ipv6` and `mihomo.dns.ipv6` to `false`, apply with `minohomo update --no-sync`, then restart the affected application. Sniffing can replace an application's original IPv4 destination with a hostname that resolves to an unreachable IPv6 endpoint. New profiles disable IPv6 by default; upgrades preserve existing settings.
- **DIRECT reports a DNS error:** check resolver reachability and bootstrap dependencies. Literal-IP DoH avoids resolving a resolver hostname through an unavailable or intercepted OS DNS path.
- **Tailscale names fail:** check that Tailscale is running, the resolver uses `#tailscale0`, and the fake-IP filter includes `+.ts.net`.
- **REALITY authentication fails:** verify the public key, short ID, SNI, fingerprint, and ML-KEM flag against the same source link.
- **Listener fails to bind:** stop other clients using the same port or change `mihomo.mixed-port`.

Make fixes in `~/.config/minohomo/config.yaml` or `rules.yaml`, then regenerate. Editing only the generated file loses changes on the next update.

## Core download or installation fails

On Debian/Arch, first-use setup prefers `/usr/share/minohomo/mihomo`. If both that bundle and `/var/lib/minohomo/mihomo` are missing, the command automatically downloads a core. If the bundle exists but cannot run, reinstall the matching architecture package or run `minohomo core update` explicitly. Source installs should also install `target/release/mihomo` at the bundle path.

The steps below apply to explicit core updates and automatic missing-core downloads.

The updater prints manual recovery steps with the selected endpoints and artifact name. Check connectivity to both the version endpoint and the release archive. If needed, configure a trusted [download mirror](configuration.md#download-mirrors).

A download, unpack, or version-check failure leaves the installed binary unchanged. An error after the move does not roll it back. Download the archive matching your machine from [Mihomo releases](https://github.com/MetaCubeX/mihomo/releases), unpack it, then install the extracted executable:

```sh
sudo install -d -m 755 /var/lib/minohomo
sudo mv -- /path/to/extracted/mihomo /var/lib/minohomo/mihomo
sudo chown root:root /var/lib/minohomo/mihomo
sudo chmod 755 /var/lib/minohomo/mihomo
minohomo update --no-sync
```

Use `minohomo update` instead if no source cache exists yet. For an already configured runtime, use `sudo systemctl restart minohomo.service` to run the new core. Check it with `minohomo core version`.

## Apply or restart fails

```sh
sudo systemctl status minohomo.service
sudo journalctl -u minohomo.service -b -n 150 --no-pager
```

A local validation failure leaves the saved config and source state unchanged. A later privileged apply failure leaves the new local config and cache available for retry. System validation failure preserves the installed config and database. A restart failure leaves the new installed config in place.

Retry deployment without fetching subscriptions:

```sh
minohomo update --no-sync
```

To retry an existing local config without regenerating it:

```sh
sudo minohomo apply --config "$HOME/.config/minohomo/current.yaml" \
  --geosite "$HOME/.config/minohomo/GeoSite.dat"
```

Omit `--geosite` if no local database exists or you want to preserve the installed copy. Packaged builds prefer the local core bundle. If both local cores are missing, privileged apply uses `--core-version-url URL` and `--core-download-url TEMPLATE` for its download fallback. The helper does not read your user settings as root.

## API unavailable or proxy not listed

Apply a config with `minohomo update` before using live controls. Check that the service is active and the running config matches the controller address and secret in the local generated config. `minohomo controller` prints those values, including the secret; do not post its output publicly.

`switch` requires a node in the running `PROXY` group. Use `minohomo list --full` and `minohomo switch` to inspect saved and running names. Apply cached sources after source edits or renames.

## Tray missing or service action denied

The tray needs a graphical session, a systemd user manager, and StatusNotifier/AppIndicator support. GNOME needs a compatible AppIndicator extension. Service actions also need a desktop polkit authentication agent.

```sh
minohomo tray sync
systemctl --user status minohomo-tray.service minohomo-tray-config.path
journalctl --user -u minohomo-tray.service -u minohomo-tray-config.service
```

The tray remains usable when the core is stopped. Automatic user-service management uses only the default profile. For another profile, run `minohomo --config-dir /absolute/path tray run` in the foreground.

If **Open dashboard** fails, check `default_terminal` and confirm that the terminal emulator is installed. The tray shows launch errors in its menu. Errors after startup appear in the user journal. See [terminal preferences](configuration.md#tray-and-terminal-preferences).

## Dashboard or editor problems

The dashboard requires interactive stdin/stdout and a terminal at least 64 columns by 22 rows. Use a CLI subcommand in scripts or redirected sessions.

Editors use `$VISUAL`, then `$EDITOR`, then `sensible-editor`. GUI editors need a wait flag, such as `EDITOR='code --wait'`, so the dashboard resumes after editing. Commands support quoted arguments, not shell pipelines or expansions.

## Geosite validation fails

A custom database must include the categories used by your DNS policy and routing rules. Defaults require `cn`, `category-ru`, `alibaba`, and `tencent`. Use `minohomo geosite status` to inspect the local database. For first setup, `minohomo geosite update --no-reload` downloads it without trying to deploy a missing local config.

## Another command holds the lock

Let the active update or apply finish before retrying. The dashboard and tray do not hold the source-cache lock between actions. Do not delete lock files while a command is running.
