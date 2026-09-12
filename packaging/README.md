# Packaging

One package, `megadl-rs`, carrying both binaries (`megadl` and `megadl-tui`).
The `packages`, `homebrew` and `pacman` jobs in
[`.github/workflows/release.yml`](../.github/workflows/release.yml) substitute
the `@VERSION@`, `@SHA_*@` and `@ARCH@` placeholders with the tag's version and
the checksums of the archives that release just built, then publish the
result. None of the definitions compiles the project from source — they all
install the binaries the `binaries` job already produced, which is what keeps
the packages and the release byte-identical.

| File | Publishes to |
| --- | --- |
| `homebrew/megadl-rs.rb` | `apiplant/homebrew-tap`, as `Formula/megadl-rs.rb` |
| `pacman/PKGBUILD` | the release itself, as a `.pkg.tar.zst` asset, and `apiplant/pacman` |
| `debian/control` | the release itself, as `.deb` assets |

This repository reuses the shared `apiplant` repositories that already serve
`nvidia-smi-live` and `portward` — `apiplant/homebrew-tap`,
`apiplant/pacman` (served at `apiplant.github.io/pacman`) and `apiplant/apt`
(served at `apt.apiplant.com`) — rather than standing up new ones. The same
repository secrets those releases use (`HOMEBREW_TAP_TOKEN`,
`PACMAN_REPO_TOKEN` + `PACMAN_GPG_PRIVATE_KEY`/`PACMAN_GPG_PASSPHRASE`,
`APT_REPO_TOKEN` + `APT_GPG_PRIVATE_KEY`/`APT_GPG_PASSPHRASE`) work here
unchanged; if this repository doesn't have them set yet, copy them over from
one of the others as repository secrets. A publish job whose credential is
absent is skipped rather than failing the release, so a fork — or this
repository before the secrets are copied — still gets a clean release.

## Platform matrix

| Platform | How it ships |
| --- | --- |
| macOS (Apple Silicon, `aarch64-apple-darwin`) | archive + Homebrew formula |
| Linux x86_64 (`x86_64-unknown-linux-gnu`) | archive, `.deb`, Arch package, Homebrew formula |
| Linux aarch64 (`aarch64-unknown-linux-gnu`) | archive, `.deb`, Homebrew formula |

The Arch package is x86_64 only: it would need an arm runner or emulation to
build, and the Arch repository has no aarch64 audience. The `.deb` and the
plain archive still cover Linux arm64.

The order is: build every archive, build the distro packages from those
archives, publish the release, then publish to Homebrew and pacman — the
formula and PKGBUILD reference the release assets by URL and would checksum a
404 otherwise.

## Using the packages

```bash
brew install apiplant/tap/megadl-rs
sudo pacman -Sy megadl-rs      # after adding apiplant.github.io/pacman as a repo
sudo dpkg -i megadl-rs_*_amd64.deb
```

Or just download and unpack the archive for your platform from the release
page — both binaries are static enough to run from anywhere, no
installation required.
