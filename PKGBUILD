# Maintainer: Huandney <huandney@gmail.com>
pkgname=snapgroup
pkgver=0.1.0
pkgrel=1
pkgdesc="Wrapper Snapper com snapshots agrupados por subvolume (save/undo/redo/list/delete/gc)"
arch=('x86_64')
url="https://github.com/huandney/snapgroup"
license=('MIT')
depends=('snapper' 'btrfs-progs' 'util-linux' 'fzf')
makedepends=('cargo')
install=snapgroup.install
# Build local: rode `makepkg -si` na raiz do repo. Sem source remoto por enquanto.
options=(!debug)

build() {
  cd "$startdir"
  cargo build --release --locked
}

package() {
  install -Dm755 "$startdir/target/release/snapg" "$pkgdir/usr/bin/snapg"
  install -Dm644 "$startdir/systemd/snapg-cleanup.service" \
    "$pkgdir/usr/lib/systemd/system/snapg-cleanup.service"
}
