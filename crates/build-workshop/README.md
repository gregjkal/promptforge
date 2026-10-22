# build-workshop

This crate is `cargo workshop`, the one build path for Runvil's desktop products. CI's `installers.yml` runs the same commands described here, so a local build makes what a release ships.

## Modes

- `cargo workshop [--release] [--target <triple>]` builds the gateway and then Workshop with Cargo. For a Windows target it stages the gateway as Workshop's sidecar around the Workshop build and removes it afterward.
- `cargo workshop installer [--target <triple>] [--sign]` builds one platform's installer in release.
- `cargo workshop manifest --assets <dir> --base-url <url> [--public-key <file>]` checks that a directory holds exactly one release's files for every platform, verifies each updater signature, and writes `latest.json` and `SHA256SUMS` beside them.
- `cargo workshop sidecar stage|remove --target <windows-triple>` stages or removes the sidecar on its own, for CI jobs that build the gateway themselves.

`cargo workshop --help` prints the full usage.

## Building an installer

Every platform needs Node 22 and the two UI installs first:

```text
npm ci --prefix crates/workshop
npm ci --prefix crates/gateway/config-ui/ui
```

The first brings the pinned Tauri CLI, and the second the gateway's Settings page, which the release build includes.

The output lands in `target/installer/<triple>/`:

- `publish/`: the installer, and under `--sign` the updater files and their `.sig` signatures. Windows gets `Runvil_<version>_x64-setup.exe`, macOS `Runvil_<version>_<aarch64|x86_64>-setup.dmg`, and Linux `Runvil_<version>_<amd64|aarch64>-setup.run`.
- `payload/` (macOS and Linux): the installed items, `Runvil.app` and `Runvil Gateway.app`, or `Runvil.AppImage` and `runvil-gateway`.
- `ifw/` (macOS and Linux): the Qt IFW definition filled in from `ifw/` in this crate, with the payload copied into its packages.

Windows needs nothing more: Tauri builds the NSIS setup from `crates/workshop/desktop/installer.nsi`, with the gateway inside.

Linux also needs Tauri's build packages and the libraries the Qt IFW tools load even without a display, as `installers.yml` installs them on Ubuntu:

```text
libwebkit2gtk-4.1-dev libssl-dev librsvg2-dev xdg-utils
libxkbcommon-x11-0 libxcb-icccm4 libxcb-image0 libxcb-keysyms1 libxcb-randr0
libxcb-render-util0 libxcb-shape0 libxcb-shm0 libxcb-sync1 libxcb-xfixes0
libxcb-xkb1 libxcb-util1 libfontconfig1 libfreetype6 libdbus-1-3
```

Where FUSE is unavailable, such as in a container, set `APPIMAGE_EXTRACT_AND_RUN=1` so Tauri's AppImage tools run without it.

### Qt Installer Framework

macOS and Linux installers need Qt IFW 4.11.0 exactly; the build checks `installerbase --version` and refuses any other version. Download the archive for your machine from `https://download.qt.io/online/qtsdkrepository/`:

- macOS (one universal build for both architectures): `mac_x64/ifw/tools_ifw_411/qt.tools.ifw.411/4.11.0-0-202603311245ifw-mac-universal.7z`
- Linux x64: `linux_x64/ifw/tools_ifw_411/qt.tools.ifw.411/4.11.0-0-202603231357ifw-linux-x64.7z`
- Linux ARM64: `linux_arm64/ifw/tools_ifw_411/qt.tools.ifw.411/4.11.0-0-202603231357ifw-linux-arm64.7z`, which runs only on glibc 2.38 or newer

Check its SHA-256 against the `ifw-sha256` pinned beside the same path in `.github/workflows/installers.yml`. Extract it into a directory you own, with `bsdtar -xf` on macOS or `7z x` on Linux, and point `QTIFWDIR` at the extracted directory that holds `bin/`. Without `QTIFWDIR`, the build uses the first `binarycreator` on `PATH` and the `installerbase` beside it.

### Signing

Without `--sign`, the build applies `crates/workshop/desktop/tauri.unsigned.conf.json` and makes no updater files. With it, the build signs Workshop's updater files and the gateway's update archive with the minisign key in `TAURI_SIGNING_PRIVATE_KEY` (the key's contents, not a path) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. For a local try, make a throwaway key:

```text
node crates/workshop/node_modules/@tauri-apps/cli/tauri.js signer generate --ci -w <dir>/run.key
```

Installed copies trust only the release key, so files signed with any other key never install as an update.
