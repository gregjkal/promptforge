# workshop

- Unsafe is confined to the Windows bridge (`src/bridge.rs`): dense working COM with documented failure modes and the crate's only unsafe code; its module-level `#[expect(unsafe_code)]` is deliberate. No other module contains unsafe code.
- The desktop app does not read Gateway configuration, own the Gateway discovery file, or kill the Gateway as part of ordinary teardown.
- Workshop carries the Gateway inside its bundle only on Windows: only `tauri.windows.conf.json` declares `externalBin`. On macOS and Linux the Gateway is its own install beside Workshop, found through `gateway-api-discovery`'s installed-peer lookup.
- The desktop app never replaces the Gateway's files. It reaches a Gateway update only by asking the running Gateway, through the `update_gateway` command and the Gateway's `POST /update`, and a debug build never asks.
- In `installer.nsi`, the Speech to Text section has no payload: the hidden `-GatewayInit` section runs `runvil-gateway init`, and never under `/UPDATE`. `/COMPONENTS=` and the exit codes 2 (a refused component list) and 3 (a failed `init`) exist for CI; `.github/installers/cell.ps1` asserts them, so a change to either updates it.
- `tauri.unsigned.conf.json` is for local builds without `--sign` only; every CI build signs.
