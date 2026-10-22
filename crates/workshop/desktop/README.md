# Workshop desktop app

This crate is the Workshop desktop app, `runvil-workshop`: a Tauri window around Workshop's server and page. It attaches to a gateway already running for the user, launches the installed one when none is, or uses the remote gateway that `~/.runvil/workshop.toml` names.

## How it ships

The Runvil installer offers Workshop, the gateway, and Speech to Text as separate components.

- Windows: one NSIS setup, built from `installer.nsi`, installs into `%LOCALAPPDATA%\Runvil`. The gateway, `runvil-gateway.exe`, travels inside Workshop's bundle and sits beside `runvil-workshop.exe`.
- macOS: a Qt IFW installer in a DMG places `Runvil.app` and `Runvil Gateway.app` side by side, in `/Applications/Runvil` by default.
- Linux: a Qt IFW `.run` installer places `Runvil.AppImage` and `runvil-gateway` side by side, in `~/Runvil` by default.

On every platform Workshop finds the gateway beside its own install through `gateway-api-discovery`'s installed-peer lookup, and the gateway's tray finds Workshop the same way.

## Updates

Workshop checks for an update a few seconds after it starts and from Help, About, through the Tauri updater and the `latest.json` on the `workshop-latest` release, whose files are signed. On macOS and Linux it replaces its own `.app` or AppImage; on Windows it runs the new setup, which updates every installed component. Once its own check is done, a release build asks the local gateway it uses to update itself. On macOS and Linux the gateway applies any newer release from the same `latest.json`; on Windows it declines, since Workshop's setup already updates it.

## Building

`cargo workshop` builds the gateway and this app for development, and `cargo workshop installer` builds a platform's installer; `crates/build-workshop/README.md` covers both.
