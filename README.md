# gog-conjure

Cross-platform desktop app that summons your owned [GOG](https://www.gog.com) games to a folder you choose.

- Log in with GOG in a login window (authorize → app captures the code automatically)
- Browse your library, checkbox-select OS/language installers and optional extras
- Queue downloads with progress bars into `{download_root}/{Game Name}/`
- Plan and burn multi-disc DVD / Blu-ray data sets (Linux `xorriso`, Windows IMAPI2, macOS `drutil`)

## Why not “open my browser like SourceGit”?

SourceGit/Gitea register a **localhost** OAuth redirect, so the system browser can bounce back to the app.

GOG’s public Galaxy client only allows:

`https://embed.gog.com/on_login_success?origin=client`

A localhost redirect returns `redirect_uri_mismatch`. So gog-conjure opens a **separate login process** with an embedded browser, uses that registered redirect, and reads `code=` from the navigation — no paste, and the main window stays open.

## Requirements

- Rust 1.75+ (edition 2021)
- Desktop session (X11 or Wayland)
- **Linux:** WebKitGTK for the login window
- **Linux (burning):** [`xorriso`](https://www.gnu.org/software/xorriso/) (libburnia). The Burn tab can install it for you via your distro’s package manager (`pkexec`/`sudo`), or you can place a binary at `vendor/xorriso` next to the app.
- **Windows (burning):** Built-in [IMAPI2](https://learn.microsoft.com/en-us/windows/win32/imapi/burning-a-disc) (no extra install). Builds a temp ISO on disk, then burns it.
- **macOS (burning):** Built-in [`drutil`](https://keith.github.io/xcode-man-pages/drutil.1.html) (DiscRecording). Stages a layout directory, then burns ISO9660 + Joliet.

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-0 xorriso
# building from source:
sudo apt install pkg-config build-essential libwebkit2gtk-4.1-dev libgtk-3-dev
```

## Run

```bash
cargo run --release
```

Click **Login with GOG**, sign in in the popup, done.

## Disc burn

On the **Library** tab, check games and use **Download** / **Download selected**, or **Plan** for a simplified disc flow:

1. **Plan** opens a modal: pick one media size for every disc, filter by OS / language / extras, and choose download now or later
2. The estimate uses **GOG file sizes** (downloads do not need to be finished yet) and shows how many identical discs you need
3. **Add to burn** creates those discs on the Burn tab (and optionally queues the filtered downloads)

Already-downloaded games (and previously burned ones) also show on the **Burn** tab so you can add or re-add them anytime.

Then on **Burn**:

1. Add games to the burn list from **Downloaded** (or the Library actions)
2. **Add disc** for each blank you have — each disc can use a different media size and its own burn settings
3. **Plan** — packs the burn list onto your discs as efficiently as possible using GOG/file sizes (optional GOG installer bin splitting). Incomplete downloads are included in the layout; **Burn** stays disabled until those downloads finish
4. Click **Burn** on each disc when ready (reburn allowed if a write fails)

Downloaded / burned status is remembered across sessions. Volume labels default to truncated game titles (ISO 9660, 32 characters).

**Burn backends**

| Platform | Backend | Notes |
|----------|---------|--------|
| Linux | `xorriso` | Path maps directly to the drive (no intermediate ISO). Search order: `vendor/xorriso` next to the binary, `xorriso` next to the binary, the same under the cwd, then `PATH`. |
| Windows | IMAPI2 | Built into Windows. Stages a layout, streams a temp ISO to disk (bounded buffer — not held in RAM), then burns the ISO. Needs free disk space ≈ disc size. |
| macOS | `drutil` | Built into macOS. Stages a layout directory, then `drutil burn -iso9660 -joliet …`. Simulate uses `-test`. |

## CI / releases

Pushing to `master` only runs build/test CI. Releases with attached binaries are created by the **Release** workflow when you push a version tag (or run it manually via `workflow_dispatch`):

```bash
git tag v0.1.0
git push origin v0.1.0
```

Workflows:

- [`.github/workflows/release.yml`](.github/workflows/release.yml) — Linux / Windows / macOS binaries on GitHub
- [`.gitea/workflows/release.yml`](.gitea/workflows/release.yml) — Linux binary on Gitea (shell-only; Act image has no Node.js)

## License

MIT
