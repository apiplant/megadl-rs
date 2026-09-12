# Packaging

One package, `megadl-rs`, carrying both binaries (`megadl` and `megadl-tui`).
The `packages`, `homebrew`, `apt` and `pacman` jobs in
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
| `apt/apt-ftparchive.conf` | `apiplant/apt`, served at `apt.apiplant.com` |

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
| Linux x86_64 (`x86_64-unknown-linux-gnu`) | archive, `.deb` + apt repo, Arch package + pacman repo, Homebrew formula |
| Linux aarch64 (`aarch64-unknown-linux-gnu`) | archive, `.deb` + apt repo, Homebrew formula |

The Arch package is x86_64 only: it would need an arm runner or emulation to
build, and the Arch repository has no aarch64 audience. The `.deb`, the apt
repo and the plain archive still cover Linux arm64.

The order is: build every archive, build the distro packages from those
archives, publish the release, then publish to Homebrew, apt and pacman — the
formula, apt pool and PKGBUILD reference the release assets (or are built
from artifacts of that same run) and the release must exist first.

## Using the packages

macOS (Apple Silicon) and Linux, via Homebrew:

```bash
brew tap apiplant/tap
brew install apiplant/tap/megadl-rs
```

Arch Linux, via the signed pacman repository at `apiplant.github.io/pacman`
(one-time setup, then `pacman -Sy`/`-Syu` picks up new releases):

```bash
curl -sSfL https://apiplant.github.io/pacman/apiplant.gpg -o /tmp/apiplant.gpg
keyid=$(gpg --show-keys --with-colons /tmp/apiplant.gpg | awk -F: '/^pub:/ { print $5; exit }') && sudo pacman-key --add /tmp/apiplant.gpg && sudo pacman-key --finger "$keyid" && sudo pacman-key --lsign-key "$keyid"
printf '\n[apiplant]\nSigLevel = Required DatabaseOptional\nServer = https://apiplant.github.io/pacman/$arch\n' | sudo tee -a /etc/pacman.conf > /dev/null
sudo pacman -Sy megadl-rs
```

Debian/Ubuntu, via the signed apt repository at `apt.apiplant.com` (one-time
setup, then `apt upgrade` picks up new releases):

```bash
curl -sSfL https://apt.apiplant.com/apiplant-archive-keyring.gpg | sudo tee /usr/share/keyrings/apiplant.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/apiplant.gpg] https://apt.apiplant.com stable main" | sudo tee /etc/apt/sources.list.d/apiplant.list > /dev/null
sudo apt update && sudo apt install megadl-rs
```

Or install a single `.deb`/`.pkg.tar.zst` release asset directly without
adding a repository:

```bash
sudo dpkg -i megadl-rs_*_amd64.deb
sudo pacman -U megadl-rs-*-x86_64.pkg.tar.zst
```

Or just download and unpack the archive for your platform from the release
page — both binaries are static enough to run from anywhere, no
installation required.
