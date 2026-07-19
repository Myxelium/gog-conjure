# gog-conjure

Cross-platform desktop app that summons your owned [GOG](https://www.gog.com) games to a folder you choose.

- Log in with GOG in a login window (authorize → app captures the code automatically)
- Browse your library, checkbox-select OS/language installers and optional extras
- Queue downloads with progress bars into `{download_root}/{Game Name}/`
- Plan future DVD / Blu-ray burns with size-aware game suggestions

## Why not “open my browser like SourceGit”?

SourceGit/Gitea register a **localhost** OAuth redirect, so the system browser can bounce back to the app.

GOG’s public Galaxy client only allows:

`https://embed.gog.com/on_login_success?origin=client`

A localhost redirect returns `redirect_uri_mismatch`. So gog-conjure opens a **separate login process** with an embedded browser, uses that registered redirect, and reads `code=` from the navigation — no paste, and the main window stays open.

## Requirements

- Rust 1.75+ (edition 2021)
- Desktop session (X11 or Wayland)
- **Linux:** WebKitGTK for the login window

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-0
# building from source:
sudo apt install pkg-config build-essential libwebkit2gtk-4.1-dev libgtk-3-dev
```

## Run

```bash
cargo run --release
```

Click **Login with GOG**, sign in in the popup, done.

## Disc burn (preview)

The **Burn** tab suggests game folders that fit DVD-5/9 or Blu-ray 25/50/100 GB. Burning itself is stubbed in `src/disc`.

## CI / releases

Tag `v0.1.0` publishes binaries via:

- [`.github/workflows/release.yml`](.github/workflows/release.yml)
- [`.gitea/workflows/release.yml`](.gitea/workflows/release.yml)

## License

MIT
