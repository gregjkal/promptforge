// Unit test for Help > About (src/parts/chrome/chrome.contribution.ts and
// src/parts/chrome/about-dialog.ts): the action resolves the registered
// update service, so the desktop dialog offers the update check instead of
// "Desktop updates unavailable", and without a registered service it still
// opens with the button disabled. Bundles the modules with esbuild and
// drives them against jsdom with a stub-backend UpdateService.
// Run: node test/about-dialog.mjs
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";
import { JSDOM } from "jsdom";

const uiDir = path.dirname(fileURLToPath(import.meta.url));

const dom = new JSDOM("", { url: "http://127.0.0.1:7910/" });
const { window } = dom;
globalThis.window = window;
globalThis.document = window.document;
globalThis.HTMLElement = window.HTMLElement;
globalThis.Element = window.Element;
globalThis.Node = window.Node;
globalThis.KeyboardEvent = window.KeyboardEvent;

const bundle = await esbuild.build({
  stdin: {
    contents: `
      import "./src/parts/chrome/chrome.contribution.ts";
      export { Commands } from "@workshop/platform/command-registry";
      export { registerService } from "@workshop/platform/service-registry";
      export { UPDATES, UpdateService } from "./src/services/update-service.ts";
    `,
    resolveDir: path.join(uiDir, ".."),
    loader: "ts",
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "browser",
  target: "es2022",
  logLevel: "silent",
  loader: { ".css": "empty" },
});
const { Commands, registerService, UPDATES, UpdateService } = await import(
  `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString("base64")}`
);

const failures = [];
function check(name, condition) {
  if (!condition) failures.push(name);
}

const ABOUT = "workbench.action.showAboutDialog";

function updateButton() {
  return window.document.querySelector(".ws-about-dialog__check");
}

// Dismisses through the Close button, as a user does, so the dialog's
// teardown runs and the next open is not a no-op.
function closeDialog(name) {
  window.document.querySelector(".ws-about-dialog__close")?.click();
  check(name, window.document.querySelector(".ws-about-dialog-overlay") === null);
}

// No service registered: the dialog still opens, with updates unavailable.
check("the About action is registered", await Commands.execute(ABOUT));
check(
  "without an update service the button reads unavailable",
  updateButton()?.textContent === "Desktop updates unavailable" && updateButton()?.disabled === true,
);
closeDialog("Close dismisses the dialog without an update service");

// The desktop app's registered service: the button offers the check.
let checks = 0;
const updates = new UpdateService({
  desktop: true,
  supported: async () => true,
  currentVersion: async () => "0.3.0",
  check: async () => {
    checks += 1;
    return null;
  },
  relaunch: async () => undefined,
  updateGateway: async () => ({ outcome: "current" }),
});
const registration = registerService(UPDATES, () => updates);
await Commands.execute(ABOUT);
const button = updateButton();
check(
  "with the desktop update service the button offers the check",
  button?.textContent === "Check for updates" && button.disabled === false,
);
button?.click();
for (let turn = 0; turn < 10 && checks === 0; turn += 1) {
  await Promise.resolve();
}
check("the button runs the update check", checks === 1);
closeDialog("Close dismisses the dialog with the update service");
registration.dispose();
updates.dispose();

if (failures.length > 0) {
  console.error(`about-dialog: ${failures.length} failure(s)`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log("about-dialog: all assertions passed");
