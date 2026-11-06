# Project guide for coding agents

## Project and scope

Minohomo is a Linux/systemd proxy client written in Rust, edition 2024. It provides a CLI, a Ratatui dashboard, a GTK-free desktop tray, and a managed Mihomo core. These interfaces share operations; do not build separate implementations of the same command for each interface.

Read the relevant source and guide before editing. Keep changes focused on the User's request. Prefer small changes and existing helpers over new frameworks, dependencies, or layers.

## Repository map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI parsing and dispatch, subscription fetching, source operations, config generation, deployment handoff, CLI unit tests |
| `src/storage.rs` | Settings, profile initialization, source state, file permissions, atomic writes, profile locking |
| `src/links.rs` | Direct-link and subscription parsing, proxy validation |
| `src/api.rs` | Mihomo controller client, mode/proxy selection, delay checks |
| `src/apply.rs` | Privileged config/geodata validation and installation, system service controls |
| `src/core.rs` | Managed core discovery, bundle installation, explicit updater, runtime locking |
| `src/geosite.rs` | Geosite download, validation, database inspection |
| `src/terminal.rs` | Shared terminal selection and independent dashboard launching |
| `src/tray.rs` | StatusNotifier tray, menu state/callbacks, preference watcher synchronization |
| `src/tui.rs` | Dashboard state and foreground CLI/editor handoff |
| `src/tui/{input,ui,worker}.rs` | Text input, rendering, background work |
| `src/integration_tests.rs` | Source update, validation, persistence, and deployment regression tests |
| `src/assets/` | Embedded tray icon, packaged application icon, README logo/screenshot |
| `defaults/` | Initial client settings and routing policy; existing profiles are not overwritten |
| `build.rs` | Target-specific, checksum-verified Mihomo bundle download and staging |
| `packaging/` | System/user units, desktop entry, pinned core version and archive checksums |
| `Cargo.toml` | Rust dependencies/features and Debian package metadata/assets |
| `PKGBUILD` | Arch packaging of the repository checkout |
| `flake.nix` | Nix packages and NixOS module |
| `.github/workflows/` | Debian/Arch builds, checks, package smoke tests, release uploads |
| `docs/` | Usage, configuration, troubleshooting, implementation, development guides |

Keep `README.md` focused on installation and everyday use. Put detailed technical instructions in the relevant `docs/` guide. Do not add migration notes to the README unless requested.

## Preferred development workflow

1. Check the current branch and working tree. Do not assume the branch from an earlier conversation.
2. Inspect the relevant implementation, tests, defaults, and package assets.
3. Briefly explain the intended change before substantial work.
4. Make the smallest complete change. Preserve unrelated edits and user-created assets.
5. Update documentation and packaging together when commands, defaults, paths, or installed assets change.
6. Validate in proportion to the change, following the testing rules below.
7. Report what changed, what was checked, and what remains unverified. Never present an older successful run as evidence for newer changes.

Use the normal `~/.cargo` cache. Do not create a project-local `.cargo-home`. Avoid unnecessary cache deletion or `cargo clean`; bundle downloads and Rust compilation should be reused.

Keep packaging simple: one root `PKGBUILD`, one Arch workflow, one `flake.nix`. Do not introduce packaging helper scripts or extra build frameworks without a concrete need.

If a sandbox blocks a required path or operation, stop and ask for access. Do not redirect homes, caches, or container storage to work around the restriction without explicit permission.

## Validation preferences

- Honor the User's instructions for the current task. If builds or tests are prohibited, do not run them.
- When the User authorizes necessary testing, run useful targeted checks instead of repeatedly asking permission or running every available check by habit.
- Do not rerun TUI tests unless explicitly requested. The User normally checks interactive UI behavior manually.
- Documentation-only and version-only changes do not need a full build or package rebuild.
- Do not perform live privileged installation, enable/stop the host's services, change host networking, or launch real GUI windows as routine tests. Ask first or use isolated fixtures.
- Use disposable distro containers for full packaging verification when authorized. Preserve the host checkout, services, and toolchain.
- A full Debian or Arch packaging run can take roughly 3–4 minutes, longer with cold caches. Wait for completion; compilation taking a few minutes is not a failure.
- Distinguish unit tests, extracted-package smoke tests, real terminal/tray checks, and native architecture coverage. An amd64 run does not verify arm64.

Useful commands, when appropriate and authorized:

```sh
cargo fmt --all --check
cargo test --locked subscription_user_agent
cargo test --locked -- --skip tui::
cargo clippy --locked --all-targets -- -D warnings
desktop-file-validate packaging/minohomo.desktop
git diff --check
```

Use a relevant test filter instead of the example filter above for other features. Formatting edits use `cargo fmt --all`.

Normal automated tests require neither root nor a running Mihomo service. Keep real-geosite validation opt-in; see `docs/development.md`.

## Behavioral and safety constraints

### Profiles and persistence

- The default profile is `~/.config/minohomo`. `--config-dir` selects another profile, not another global runtime service.
- Profile files, source URLs, credentials, controller secrets, generated configs, caches, and backups are sensitive. Preserve owner-only permissions and avoid exposing them in logs or errors.
- Keep source IDs stable. Renaming must preserve links, cached nodes, and timestamps. Source selectors use names, including numeric-looking names.
- Preserve existing user configs and custom validators/reload commands. New optional settings need serde defaults for older config files.
- Mutating operations use the Store lock. Dashboard/tray readers must not hold that lock between actions. Release locks before spawning independent processes. Preserve explicit unlock-on-drop behavior for inherited descriptors.
- Failed fetches or validation must not commit partial source/config updates. Preserve the existing deployment failure semantics described in `docs/implementation.md`.

### CLI, dashboard, and tray

- Bare `minohomo` opens the dashboard in the current interactive terminal. `minohomo --window` opens a new terminal through the shared launcher. Do not restore the removed `open` subcommand.
- The desktop entry uses `Terminal=false`; the application selects the terminal itself. Respect `default_terminal` and its existing alias/fallback behavior.
- Launch the dashboard in an independent transient user service so disabling the tray cannot kill it.
- Preserve inherited terminal colors, UTF-8 input, terminal restoration, editor arguments, and interactive sudo/editor handoff. Parse commands without adding an implicit shell.
- Keep blocking network/service work out of rendering and D-Bus callbacks. Reuse CLI operations for foreground actions.
- Automatic tray management applies only to the default profile. Disabling the tray must leave its preference watcher available.
- Tray service controls show **Stop** while running or starting, **Start** while stopped, and **Restart** in either state. These control runtime state, not boot enablement.
- Keep `ping-url` and `subscription-user-agent` as client-level settings outside `mihomo`. Preserve their defaults when omitted.
- Delay checks must exclude exact names `GLOBAL`, `COMPATIBLE`, `PROXY`, `PASS`, `PASS-RULE`, `REJECT`, and `REJECT-DROP` before requests. Do not exclude `DIRECT` or arbitrary custom groups.

### Managed core and privilege boundaries

- Runtime files belong under `/var/lib/minohomo`, not `/etc`. The core service is `minohomo.service`.
- Core lookup order is runtime binary, installed bundle, then automatic download if both are missing. Do not replace an existing runtime core during ordinary setup. An unusable existing bundle must produce an error rather than silently downloading another core.
- `core version` must print exactly `Not installed` when the runtime core is absent, without initializing a profile.
- The privileged apply helper must not load user settings or initialize root's profile. Pass required paths and configured download endpoints explicitly across the privilege boundary.
- Preserve explicit updater installation via move, root ownership, and mode 755. Run privileged commands directly when already root. Explicit updates restart only an active core service.
- Keep bounds, redirect policies, archive handling, checksum/version checks, temporary cleanup, and recovery errors intact.

## Packaging and assets

- Debian and Arch bundle Mihomo by default. Nix disables the `bundled-core` feature and keeps download-on-first-use behavior.
- `build.rs` selects Cargo's target architecture, never the build host's CPU. Compilation must not execute a downloaded target binary. x86-64 bundles use portable `amd64-v1`.
- Update `packaging/mihomo-version` and `packaging/mihomo-sha256sums` together when changing the pin. Verify upstream artifacts and digests.
- Keep the shared bundle cache under `target/bundled-core/cache/`, keyed by target, version, and archive digest.
- Debian installs the staged core from `target/bundled-core/mihomo`. Do not change that asset to `target/release/mihomo`: cargo-deb treats that prefix as a Rust binary target.
- Arch uses profile-adjacent binaries under `target/<target-triple>/release/`. Keep makepkg's `BUILDDIR` under `target/makepkg` so cleanup cannot delete Rust's `src/`.
- Keep Arch `options=('!lto')`: makepkg's GCC LTO conflicts with the Rust LLVM linker for AWS-LC. Arch may emit a separate debug package; smoke-test the main package explicitly, not an assumed single archive.
- Debian release CI builds amd64 and arm64 in Debian 12. Arch release CI builds x86_64. Preserve separate checksum names so Arch cannot overwrite Debian's release checksums.
- Package installation must not initialize profiles, create active runtime configs, or activate services. NixOS boot activation belongs to its explicitly enabled module.
- When adding installed files, update Debian assets in `Cargo.toml`, `PKGBUILD`, `flake.nix`, and relevant workflow smoke checks.
- The application icon is `src/assets/app.png`, 256×256, installed as `hicolor/256x256/apps/minohomo.png`. `Icon=minohomo` relies on theme lookup. Do not use the unregistered `150x150` directory. The embedded tray icon is separate.
- Preserve Nix store path substitutions and the sudo wrapper path. Do not assume `/usr/bin` exists on NixOS.

## Git and releases

- Do not commit, merge, tag, push, or rewrite history unless requested. A request to merge/tag is not permission to push.
- The usual release flow is work on `dev`, bump and commit the version, merge `dev` into `main`, then create an annotated `v<VERSION>` tag on the merge commit.
- Keep versions aligned in `Cargo.toml`, the `minohomo` entry of `Cargo.lock`, and `PKGBUILD`. Do not replace matching dependency versions in the lockfile. Reset `pkgrel` to 1 for a new upstream version; the flake derives its version from Cargo.
- Use a brief changelog in the annotated tag message. Describe changes since the previous release tag, not only since the latest merge.
- Do not move an existing release tag without explicit permission. Finish by reporting the branch/commit/tag, working-tree state, and whether anything was pushed.
