# megadl-rs

A command-line downloader and terminal download manager for public
[mega.nz](https://mega.nz) links, implemented from scratch in Rust against
MEGA's own API and crypto — no `megatools` or `megacmd` dependency.

| Binary | Description |
| --- | --- |
| `megadl` | One-shot CLI downloader, aiming for flag compatibility with megatools' `megadl` rather than the Go megadl TUI's own invocation style. |
| `megadl-tui` | A terminal download manager: a queue you feed links into, downloaded one at a time, rendered with plain ANSI and no TUI framework. |

## Usage

```sh
# download one or more files/folders
megadl https://mega.nz/file/...
megadl --path=downloads/ https://mega.nz/folder/...

# pick which files to fetch from a folder link
megadl --choose-files https://mega.nz/folder/...

# fetch exactly these node handles (what --choose-files prints)
megadl --files=H1,H2 https://mega.nz/folder/...

# print each downloaded file's local path, or suppress the progress bar
megadl --print-names --no-progress https://mega.nz/file/...

# terminal download manager
megadl-tui
```

Run `megadl --help` for the full option list.

## Installation

macOS (Apple Silicon) and Linux, via Homebrew:

```sh
brew tap apiplant/tap
brew install apiplant/tap/megadl-rs
```

Arch Linux, via the signed pacman repository at `apiplant.github.io/pacman`
(one-time setup, then `pacman -Sy`/`-Syu` picks up new releases):

```sh
curl -sSfL https://apiplant.github.io/pacman/apiplant.gpg -o /tmp/apiplant.gpg
keyid=$(gpg --show-keys --with-colons /tmp/apiplant.gpg | awk -F: '/^pub:/ { print $5; exit }') && sudo pacman-key --add /tmp/apiplant.gpg && sudo pacman-key --finger "$keyid" && sudo pacman-key --lsign-key "$keyid"
printf '\n[apiplant]\nSigLevel = Required DatabaseOptional\nServer = https://apiplant.github.io/pacman/$arch\n' | sudo tee -a /etc/pacman.conf > /dev/null
sudo pacman -Sy megadl-rs
```

Debian/Ubuntu, via the signed apt repository at `apt.apiplant.com` (one-time
setup, then `apt upgrade` picks up new releases):

```sh
curl -sSfL https://apt.apiplant.com/apiplant-archive-keyring.gpg | sudo tee /usr/share/keyrings/apiplant.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/apiplant.gpg] https://apt.apiplant.com stable main" | sudo tee /etc/apt/sources.list.d/apiplant.list > /dev/null
sudo apt update && sudo apt install megadl-rs
```

Or download the archive, `.deb`, or `.pkg.tar.zst` for your platform from the
[releases page](https://github.com/apiplant/megadl-rs/releases) and install
it directly — the plain archive needs no installation at all, both binaries
are static enough to run from anywhere.

| Platform | Ships as |
| --- | --- |
| macOS (Apple Silicon) | archive, Homebrew |
| Linux x86_64 | archive, `.deb` + apt repo, Arch package + pacman repo, Homebrew |
| Linux aarch64 | archive, `.deb` + apt repo, Homebrew |

See [`packaging/README.md`](packaging/README.md) for how these packages are
built and published.

## Features

- Talks to MEGA's API directly (JSON commands, `X-Hashcash` proof-of-work,
  EAGAIN retry) — see [`src/api.rs`](src/api.rs).
- Implements MEGA's crypto itself: AES-ECB key unwrap, AES-CBC attribute
  decryption, AES-CTR file decryption, and the chunked CBC-MAC used to
  verify integrity as bytes arrive — see [`src/crypto.rs`](src/crypto.rs).
- Resumable downloads: partial files persist as `.megatmp.<handle>` next to
  the target, and a restart re-feeds the bytes already on disk through the
  chunked MAC and asks the server only for the remainder — see
  [`src/download.rs`](src/download.rs).
- Full exported-folder support: fetches and decrypts the node tree, and
  offers an interactive checkbox-tree file picker for either the CLI's
  `--choose-files` or the TUI's "add folder" flow — see
  [`src/folder.rs`](src/folder.rs) and [`src/picker.rs`](src/picker.rs).

## Building from source

```sh
cargo build --release
```

Produces `target/release/megadl` and `target/release/megadl-tui`. No
system dependencies beyond a Rust toolchain — TLS, AES and hashing are all
pure-Rust (`rustls`, `aes`, `sha2`).

## License

MIT — see [LICENSE](LICENSE).
