[![CI](https://github.com/cppalliance/runvil/actions/workflows/ci.yml/badge.svg)](https://github.com/cppalliance/runvil/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-BSL--1.0-blue.svg)](LICENSE)

# Runvil

Runvil runs AI prompt pipelines defined in a single Markdown file. The Markdown is the program, and the model is the CPU.

![Gloves and sparks](images/banner-04.png)

Workshop is the desktop app for writing prompts, running them against local or remote models, and reviewing every run it records. One installer per platform installs it with the gateway:

| Platform | Installer |
|---|---|
| Windows x64 | [Runvil-setup.exe](https://github.com/cppalliance/runvil/releases/download/workshop-latest/Runvil-setup.exe) |
| macOS, Apple silicon | [Runvil-arm64.dmg](https://github.com/cppalliance/runvil/releases/download/workshop-latest/Runvil-arm64.dmg) |
| macOS, Intel | [Runvil-x64.dmg](https://github.com/cppalliance/runvil/releases/download/workshop-latest/Runvil-x64.dmg) |
| Linux x64 (Ubuntu 22.04 or later) | [Runvil-x64.run](https://github.com/cppalliance/runvil/releases/download/workshop-latest/Runvil-x64.run) |
| Linux ARM64 (Ubuntu 24.04, Debian 13, or later) | [Runvil-arm64.run](https://github.com/cppalliance/runvil/releases/download/workshop-latest/Runvil-arm64.run) |

The [latest release](https://github.com/cppalliance/runvil/releases/latest) holds the same installers under versioned names, with a `SHA256SUMS` file that lists them by those names. On Linux, make the `.run` file executable and run it from your desktop session.

The installer offers three components. Workshop is the desktop app. Gateway is the local gateway described below, with its tray icon and Settings page. Speech to Text needs Gateway, and downloads the speech runtime and two speech models during the install, about 0.6 GB, more on Windows and Linux x64 with an NVIDIA GPU, so dictation works offline from the first launch. Workshop installed without Gateway uses the gateway that `~/.runvil/workshop.toml` names. Windows installs into `%LOCALAPPDATA%\Runvil`, macOS into `/Applications/Runvil`, and Linux into `~/Runvil`.

Workshop checks for updates when it starts and offers each new version, which it installs when you accept. After each check it also asks the local gateway it uses to update. A gateway installed without Workshop checks for updates every day, shows a new version in its tray menu, and installs it when you choose that menu item.

The installers are not code-signed yet. On Windows, SmartScreen may stop the setup: choose More info, then Run anyway. On macOS, the first open is refused: open System Settings, then Privacy & Security, choose Open Anyway, and open the installer again. Until the apps are signed, the gateway's Launch at Login does not work on macOS.

![Android heads](images/banner-02.png)

The gateway is the one process that holds model credentials. It routes each model request to a cloud provider or to a model running on the user's own machine, so no other process ever sees a vendor key.

![Holographic code](images/banner-03.png)

A prompt combines frontmatter, Lua code, and prose sections. The Engine runs it as a deterministic state machine, and the Harness performs every model call, tool call, and file access the Engine asks for. Workshop is one Host, and any other program can be a Host by running prompts through the same Harness.

![Robot internals](images/banner-05.png)

The [documentation](https://cppalliance.org/runvil/) covers the prompt language, Workshop, the gateway, and the library references.

![Filing cabinets](images/banner-06.png)

Pull requests are welcome, and every change must pass the formatting, lint, and test checks that CI runs.

![Creator](images/runvil-portrait.png)

Runvil is distributed under the [Boost Software License](LICENSE).
