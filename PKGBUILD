# Maintainer: Alex Karev <alexkarev@sjtu.edu.cn>
pkgname=minohomo
pkgver=0.1.7
pkgrel=1
pkgdesc='Linux proxy client with a terminal dashboard, desktop tray, and managed Mihomo core'
arch=('x86_64' 'aarch64')
url='https://github.com/alex-karev/minohomo'
license=('GPL-3.0-only')
# AWS-LC's GCC LTO objects cannot be linked by Rust's LLVM linker.
options=('!lto')
depends=('glibc' 'gcc-libs' 'ca-certificates' 'sudo' 'systemd')
makedepends=('cargo' 'cmake' 'curl' 'gzip')
optdepends=(
  'polkit: desktop authorization for tray service controls'
  'xdg-terminal-exec: open the dashboard in the desktop default terminal'
)
# Build the checkout. Keep makepkg cleanup away from the Rust src/ directory.
BUILDDIR="$startdir/target/makepkg"

prepare() {
  cd "$startdir"
  cargo fetch --locked --target "$CARCH-unknown-linux-gnu"
}

build() {
  cd "$startdir"
  cargo build --frozen --release --target "$CARCH-unknown-linux-gnu"
}

check() {
  cd "$startdir"
  # Desktop/TUI behavior is tested manually, not inside a package builder.
  cargo test --frozen --target "$CARCH-unknown-linux-gnu" -- --skip tui::
}

package() {
  cd "$startdir"
  install -Dm755 "target/$CARCH-unknown-linux-gnu/release/minohomo" "$pkgdir/usr/bin/minohomo"
  install -Dm755 "target/$CARCH-unknown-linux-gnu/release/mihomo" "$pkgdir/usr/share/minohomo/mihomo"
  install -Dm644 packaging/minohomo.service "$pkgdir/usr/lib/systemd/system/minohomo.service"
  install -Dm644 packaging/minohomo-tray.service "$pkgdir/usr/lib/systemd/user/minohomo-tray.service"
  install -Dm644 packaging/minohomo-tray-config.service "$pkgdir/usr/lib/systemd/user/minohomo-tray-config.service"
  install -Dm644 packaging/minohomo-tray-config.path "$pkgdir/usr/lib/systemd/user/minohomo-tray-config.path"
  install -Dm644 packaging/minohomo.desktop "$pkgdir/usr/share/applications/minohomo.desktop"
  install -Dm644 src/assets/app.png "$pkgdir/usr/share/icons/hicolor/256x256/apps/minohomo.png"
  install -Dm644 README.md "$pkgdir/usr/share/doc/minohomo/README.md"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/minohomo/LICENSE"
  install -Dm644 defaults/config.yaml "$pkgdir/usr/share/doc/minohomo/defaults/config.yaml"
  install -Dm644 defaults/rules.yaml "$pkgdir/usr/share/doc/minohomo/defaults/rules.yaml"
  local guide
  for guide in docs/*.md; do
    install -Dm644 "$guide" "$pkgdir/usr/share/doc/minohomo/$guide"
  done
}
