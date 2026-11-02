# Development

[Back to README](../README.md) · [Implementation](implementation.md)

## Building from source

Install Rust/Cargo and a C build toolchain. On Debian/Ubuntu:

```sh
sudo apt install build-essential ca-certificates cmake pkg-config
```

Clone and build:

```sh
git clone https://github.com/alex-karev/minohomo.git
cd minohomo
cargo build --release --locked
sudo install -m 755 target/release/minohomo /usr/local/bin/minohomo
```

Run as your regular user. The managed core and global service unit install when first needed. You do not need to install Mihomo separately.

Source builds do not install the packaged tray user units. To use automatic tray management, install the binary at `/usr/bin/minohomo`, where those units expect it, then install the units:

```sh
sudo install -m 755 target/release/minohomo /usr/bin/minohomo
mkdir -p ~/.config/systemd/user
install -m 644 packaging/minohomo-tray.service \
  packaging/minohomo-tray-config.service \
  packaging/minohomo-tray-config.path ~/.config/systemd/user/
systemctl --user daemon-reload
minohomo tray sync
```

Alternatively, run `minohomo tray run` in the foreground without user-unit setup. Choose one binary installation location so your shell and user units use the intended executable.

## Debian packaging

```sh
sudo apt install build-essential ca-certificates cmake dpkg-dev pkg-config
cargo install cargo-deb --version '=3.7.0' --locked
cargo deb --locked
```

Packages appear in `target/debian/`. The package contains the client, global core service unit, tray user units, icon, documentation, and default settings/rules. It does not bundle the downloaded core or create runtime config. No package scripts activate services.

The [Debian workflow](../.github/workflows/debian.yml) builds amd64 and arm64 packages in Debian 12 containers, runs tests and static checks, and inspects/smoke-tests the package. Release tags must match the Cargo version as `v<VERSION>`.

## Checks

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

For a non-TUI test run:

```sh
cargo test --locked -- --skip tui::
```

Normal automated tests need neither root nor a running daemon. Desktop integration, systemd/polkit prompts, privileged installation, and terminal-emulator behavior need separate manual checks in a suitable environment.

The optional real-database test requires an installed managed core and a real geosite file. It writes its fixtures only to a temporary directory:

```sh
MINOHOMO_TEST_GEOSITE=/path/to/GeoSite.dat \
  cargo test --locked real_database_passes_mihomo_validation_and_replacement -- --ignored --nocapture
```

## Contributions

Keep pull requests focused. Describe the behavior change, tests, and any manual checks. Include sanitized reproduction steps for bugs. Do not post source URLs, controller secrets, or generated configs containing credentials.

Update the relevant guide when changing commands, defaults, shortcuts, service behavior, or supported imports. Keep the main README focused on installation and everyday use; put technical details in `docs/`.
