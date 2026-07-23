<p align="center">
  <img src="assets/icon-512.png" alt="gog-conjure logotype" width="160" height="160"><br>
  <img alt="GitHub Downloads (all assets, all releases)" src="https://img.shields.io/github/downloads/myxelium/gog-conjure/total">
</p>

# gog-conjure

A desktop app for downloading the games you own on [GOG](https://www.gog.com) to a folder on your computer, and optionally burning them to DVD or Blu-ray discs.

It works on Linux, Windows, and macOS. You sign in with your GOG account, browse your library, pick what to download, and (if you want) let the app automatically plan how those files fit onto physical discs.

## What it does

- **Download your library** - Sign in, choose a download folder, then select games, installers (by OS and language), and extras. Downloads go into subfolders named after each game.
- **Watch progress** - The Queue tab shows what is downloading and how far along each job is.
- **Burn to disc** - Plan how games fit onto DVD or Blu-ray blanks, then burn them from the Burn tab. The app remembers what you have already downloaded or burned.

You do not need the GOG Galaxy client for these workflows.

## Getting started

### Install a release

Download a build for your system from the project’s [Releases](../../releases) page:

| Platform | Typical assets |
|----------|----------------|
| Linux | AppImage, `.deb`, or raw binary (`x86_64` / `aarch64`) |
| Windows | `.exe` (`x86_64` / `aarch64`) |
| macOS | Raw binary (`aarch64` / `x86_64`) |

On Linux, disc burning needs [xorriso](https://www.gnu.org/software/xorriso/). The Burn tab can install it for you, or you can install it yourself (for example `sudo apt install xorriso` on Debian/Ubuntu). Windows and macOS use built-in disc tools, so no extra burn software is required there.

You also need a normal desktop session (not a headless server). On Linux, the login window needs WebKitGTK (often already present; on Debian/Ubuntu: `libwebkit2gtk-4.1-0`).

### First run

1. Open the app.
2. Click **Login with GOG** and sign in in the window that appears. The app captures the login automatically — you do not paste a code.
3. Choose a **download folder** if prompted (or set one from the top bar).
4. Use the **Library** tab to browse your games.

## Using the app

### Download games

1. On **Library**, find a game (search and filters help with large libraries).
2. Open it and check the installers and extras you want, or check several games for a batch download.
3. Click **Download** or **Download selected**.
4. Follow progress on the **Queue** tab. Files land in `{your download folder}/{Game Name}/`.

### Plan and burn discs

If you want physical copies (backup or offline archive):

1. On **Library**, select games and click **Plan**, or add already-downloaded games from the **Burn** tab.
2. In the plan flow, pick a disc size (DVD / Blu-ray), filter by OS / language / extras if needed, and choose whether to download now or later. The estimate uses GOG’s file sizes, so downloads do not have to be finished yet.
3. On **Burn**, add blank discs (**Add disc**), adjust sizes or settings per disc if you like, then **Plan** to pack the burn list onto those discs.
4. When downloads for a disc are complete, click **Burn**. If a write fails, you can try again.

Incomplete downloads are included in the layout plan, but **Burn** stays disabled until those files are ready. Volume labels default to short versions of the game titles (disc filesystem limit: 32 characters).

<img width="1180" height="792" alt="image" src="https://github.com/user-attachments/assets/8e3f147b-975a-492c-8250-05000590fae0" />

<img width="1281" height="865" alt="image" src="https://github.com/user-attachments/assets/d418cb97-d973-447e-a51b-52656ecbb885" />


## Build from source

For developers or anyone compiling locally:

- Rust 1.75+ (edition 2021)
- Desktop session (X11 or Wayland on Linux)
- **Linux:** WebKitGTK for login; `xorriso` for burning (or place a binary at `vendor/xorriso` next to the app)
- **Windows burning:** IMAPI2 (built in)
- **macOS burning:** `drutil` (built in)

```bash
# Debian / Ubuntu — runtime
sudo apt install libwebkit2gtk-4.1-0 xorriso
# Debian / Ubuntu — build
sudo apt install pkg-config build-essential libwebkit2gtk-4.1-dev libgtk-3-dev

cargo run --release
```

### Burn backends (technical)

| Platform | Backend | Notes |
|----------|---------|--------|
| Linux | `xorriso` | Writes via the drive path (no intermediate ISO). Search order: `vendor/xorriso` next to the binary, `xorriso` next to the binary, the same under the cwd, then `PATH`. |
| Windows | IMAPI2 | Stages a layout, writes a temporary ISO to disk, then burns it. Needs free disk space about the size of the disc. |
| macOS | `drutil` | Stages a layout directory, then burns ISO9660 + Joliet. Simulate uses `-test`. |

## Releases and CI

Pushes to `master` run build/test CI. Versioned releases with binaries are created by the **Release** workflow when you push a version tag (or run it manually):

```bash
git tag v0.1.0
git push origin v0.1.0
```

- [`.github/workflows/release.yml`](.github/workflows/release.yml) — GitHub assets for Linux, Windows, and macOS
- [`.gitea/workflows/release.yml`](.gitea/workflows/release.yml) — Gitea Linux assets for the runner arch

## Heads up
This software is mostly tested and built for my Linux Ubuntu operating system but should work for Windows and Mac but haven't been tested on those systems. Burning discs might be the failing point since I haven't tested that enough.

## License

MIT
