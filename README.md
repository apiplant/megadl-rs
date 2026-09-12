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
