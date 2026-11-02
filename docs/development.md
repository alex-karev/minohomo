# Development

[Back to README](../README.md) · [Implementation](implementation.md)

## Building from source

Install Rust/Cargo and a C build toolchain. On Debian/Ubuntu:

```sh
sudo apt install build-essential ca-certificates cmake pkg-config curl gzip
```

Clone and build:

```sh
git clone https://github.com/alex-karev/minohomo.git
cd minohomo
cargo build --release --locked
sudo install -m 755 target/release/minohomo /usr/local/bin/minohomo
sudo install -Dm755 target/release/mihomo /usr/share/minohomo/mihomo
```

The default `bundled-core` Cargo feature downloads the pinned Mihomo release for Cargo's target architecture during compilation. It verifies the archive against `packaging/mihomo-sha256sums` and unpacks it. The shared cache lives under `target/bundled-core/cache/`, keyed by target triple, pinned version, and archive checksum. Debug/release builds and changed build-script fingerprints reuse that cache without downloading again. A full `cargo clean` removes it; a different target, version, or checksum uses a new entry. Cross-builds select the target architecture, not the host CPU. x86-64 bundles use the portable `amd64-v1` artifact.

The bundled release is pinned in `packaging/mihomo-version`. When updating that pin, update the corresponding archive checksums too. The current bundle uses [Mihomo v1.19.33](https://github.com/MetaCubeX/mihomo/tree/v1.19.33). Builds need HTTPS access to GitHub only when the matching shared cache entry is missing. The build stages `target/bundled-core/mihomo` for cargo-deb and also places `mihomo` beside the client for source and Arch installs. The Debian asset sits outside `target/release/` because cargo-deb treats files under that prefix as Cargo binary targets.

Run as your regular user. First use copies the installed bundle into `/var/lib/minohomo` and installs the global service unit if needed. If neither the runtime core nor the installed bundle exists, commands that need the core download it automatically. Source installs should install both binaries as shown above.

Use `cargo build --release --locked --no-default-features` to disable bundling and retain the download-on-first-use workflow. This is what the Nix flake uses.

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
sudo apt install build-essential ca-certificates cmake dpkg-dev pkg-config curl gzip
cargo install cargo-deb --version '=3.7.0' --locked
cargo deb --locked
```

Packages appear in `target/debian/`. The package contains the client, global core service unit, tray user units, icon, documentation, and default settings/rules. It also includes the target-architecture core at `/usr/share/minohomo/mihomo`, but creates no runtime config. No package scripts activate services.

The [Debian workflow](../.github/workflows/debian.yml) builds amd64 and arm64 packages in Debian 12 containers, runs tests and static checks, and inspects/smoke-tests the package. Release tags must match the Cargo version as `v<VERSION>`.

## Arch packaging

The root `PKGBUILD` builds the repository checkout, not a separately downloaded release archive:

```sh
git clone https://github.com/alex-karev/minohomo.git
cd minohomo
makepkg -si
```

It installs the same client, bundled core, units, tray icon, defaults, and guides as the Debian package. It does not download the core or activate services during package installation. Native x86_64 and aarch64 builds are supported; release CI builds x86_64 using the official Arch Linux container.

The [Arch workflow](../.github/workflows/arch.yml) builds and tests packages on pushes and pull requests. Version tags publish `.pkg.tar.zst` assets and `SHA256SUMS-archlinux` to GitHub Releases. Keep `pkgver` in `PKGBUILD` aligned with `Cargo.toml`; increment `pkgrel` for packaging-only revisions.

## NixOS

The Nix flake disables the `bundled-core` feature, so its sandbox never downloads a core during compilation. NixOS keeps download-on-first-use behavior.

The root `flake.nix` exports a default package and `nixosModules.default` for x86_64-linux and aarch64-linux. Add it to your existing NixOS flake:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    minohomo.url = "github:alex-karev/minohomo";
    minohomo.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, minohomo, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        minohomo.nixosModules.default
        { services.minohomo.enable = true; }
      ];
    };
  };
}
```

Replace `myhost` and the system architecture as needed, then rebuild your NixOS configuration. The module registers system and user units, enables core startup at boot, and creates `/var/lib/minohomo`. Startup waits until the managed binary and config exist. Run `minohomo` as your regular user and update your sources to complete setup. Use the NixOS module for boot activation instead of `systemctl enable`.

The Nix package patches tool and unit paths to Nix store locations and uses `/run/wrappers/bin/sudo` for privileged commands. The managed core and runtime files remain in `/var/lib/minohomo`; core updates do not write to the Nix store. Per-user tray preferences work as usual.

To build the client without enabling the module:

```sh
nix build .
nix run . -- --help
```

The module is required to register the service on NixOS. Commit the `flake.lock` generated by Nix when pinning inputs for a deployment.

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
