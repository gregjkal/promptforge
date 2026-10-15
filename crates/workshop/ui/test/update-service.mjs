import path from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";
import { assertNoLeaks } from "./helpers/leak-check.mjs";

const uiDir = path.dirname(fileURLToPath(import.meta.url));
const bundle = await esbuild.build({
  stdin: {
    contents: `
      export * as lifecycle from "@workshop/platform/lifecycle";
      export { UpdateService } from "./src/services/update-service.ts";
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
});
const code = bundle.outputFiles[0].text;
const { lifecycle, UpdateService } = await import(
  `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`
);

const failures = [];
const check = (name, condition) => {
  if (!condition) failures.push(name);
};

await assertNoLeaks(lifecycle, async () => {
  let relaunched = 0;
  let closed = 0;
  const update = {
    currentVersion: "0.2.0",
    version: "0.3.0",
    body: "Faster startup\nMore details",
    async downloadAndInstall(onEvent) {
      onEvent({ event: "Started", data: { contentLength: 100 } });
      onEvent({ event: "Progress", data: { chunkLength: 40 } });
      onEvent({ event: "Progress", data: { chunkLength: 60 } });
      onEvent({ event: "Finished" });
    },
    async close() {
      closed += 1;
    },
  };
  const service = new UpdateService({
    desktop: true,
    supported: async () => true,
    currentVersion: async () => "0.2.0",
    check: async () => update,
    relaunch: async () => {
      relaunched += 1;
    },
    updateGateway: async () => ({ outcome: "current" }),
  });
  await service.checkNow();
  check("a newer release becomes available", service.snapshot.phase === "available");
  check("the available version is retained", service.snapshot.version === "0.3.0");
  check("only the first release-note line is shown", service.snapshot.notes === "Faster startup");
  service.remindLater();
  check("remind later dismisses the banner state", service.snapshot.phase === "dismissed");
  service.showAvailable();
  check("a dismissed update can be shown again", service.snapshot.phase === "available");
  await service.install();
  check("the completed update reaches restart", service.snapshot.phase === "restarting");
  check("download chunks accumulate against the total", service.snapshot.downloaded === 100);
  check("the process relaunch is requested", relaunched === 1);
  service.dispose();
  await Promise.resolve();
  check("disposing closes the native update handle", closed === 1);

  let checked = 0;
  const deb = new UpdateService({
    desktop: true,
    supported: async () => false,
    currentVersion: async () => "0.2.0",
    check: async () => {
      checked += 1;
      return null;
    },
    relaunch: async () => undefined,
    updateGateway: async () => {
      checked += 1;
      return { outcome: "current" };
    },
  });
  await deb.checkNow();
  check("package-managed Linux disables in-app updates", deb.snapshot.phase === "unsupported");
  check("an unsupported package never checks the updater endpoint", checked === 0);
  deb.dispose();

  // The gateway's update follows every Workshop check result, once.
  const gatewayService = (check, gateway) => {
    const calls = { gateway: 0 };
    const service = new UpdateService({
      desktop: true,
      supported: async () => true,
      currentVersion: async () => "0.2.0",
      check,
      relaunch: async () => undefined,
      updateGateway: async () => {
        calls.gateway += 1;
        return gateway();
      },
    });
    return { service, calls };
  };
  const results = {
    current: async () => null,
    available: async () => ({ ...update, async close() {} }),
    failed: async () => {
      throw new Error("offline");
    },
  };
  for (const [result, workshopCheck] of Object.entries(results)) {
    const { service: after, calls } = gatewayService(workshopCheck, () => ({ outcome: "current" }));
    await after.checkNow();
    check(`the gateway updates once after a ${result} check`, calls.gateway === 1);
    check(
      `the gateway's answer is recorded after a ${result} check`,
      after.snapshot.gateway?.outcome === "current",
    );
    after.dispose();
  }

  {
    const { service: quiet } = gatewayService(results.failed, () => ({ outcome: "predates" }));
    await quiet.checkNow();
    check("a predating gateway is recorded", quiet.snapshot.gateway?.outcome === "predates");
    check("a predating gateway leaves Workshop's phase", quiet.snapshot.phase === "error");
    check("a predating gateway leaves Workshop's error", quiet.snapshot.error === "offline");
    check(
      "a predating gateway adds nothing to the log",
      quiet.snapshot.log.join("|") === "Update check failed: offline",
    );
    quiet.dispose();
  }

  {
    const { service: updating } = gatewayService(results.current, () => ({
      outcome: "updating",
      version: "0.4.0",
    }));
    await updating.checkNow();
    check("an updating gateway leaves Workshop's phase", updating.snapshot.phase === "current");
    check(
      "an updating gateway adds its log line",
      updating.snapshot.log.join("|") === "PromptForge Gateway is updating to 0.4.0.",
    );
    updating.dispose();
  }

  {
    const { service: rejected } = gatewayService(results.current, () => {
      throw new Error("command update_gateway not allowed");
    });
    let thrown = false;
    try {
      await rejected.checkNow();
    } catch {
      thrown = true;
    }
    check("a rejected gateway update never throws", !thrown);
    check(
      "a rejected gateway update is recorded as failed",
      rejected.snapshot.gateway?.outcome === "failed" &&
        rejected.snapshot.gateway.message === "command update_gateway not allowed",
    );
    check("a rejected gateway update leaves Workshop's phase", rejected.snapshot.phase === "current");
    check("a rejected gateway update leaves Workshop's error", rejected.snapshot.error === "");
    check(
      "a rejected gateway update logs its failure",
      rejected.snapshot.log.join("|") ===
        "PromptForge Gateway update failed: command update_gateway not allowed",
    );
    rejected.dispose();
  }

  {
    let gatewayCalls = 0;
    const browser = new UpdateService({
      desktop: false,
      supported: async () => true,
      currentVersion: async () => "0.2.0",
      check: async () => null,
      relaunch: async () => undefined,
      updateGateway: async () => {
        gatewayCalls += 1;
        return { outcome: "current" };
      },
    });
    await browser.checkNow();
    check("the browser never asks the gateway to update", gatewayCalls === 0);
    check("the browser records no gateway answer", browser.snapshot.gateway === null);
    browser.dispose();
  }

  {
    // The gateway answers only after its download, so a manual check while
    // it downloads still runs Workshop's check, and joins the request.
    let answerGateway;
    let workshopChecks = 0;
    const { service: slow, calls } = gatewayService(
      async () => {
        workshopChecks += 1;
        return null;
      },
      () => new Promise((resolve) => (answerGateway = resolve)),
    );
    const first = slow.checkNow();
    for (let turn = 0; turn < 10 && answerGateway === undefined; turn += 1) {
      await Promise.resolve();
    }
    check("the gateway request is in flight", answerGateway !== undefined);
    const phases = [];
    const listener = slow.onDidChange((snapshot) => phases.push(snapshot.phase));
    const second = slow.checkNow();
    for (let turn = 0; turn < 10 && phases.at(-1) !== "current"; turn += 1) {
      await Promise.resolve();
    }
    check("a check during the gateway request runs Workshop's check", workshopChecks === 2);
    check("a check during the gateway request shows its progress", phases.join(",") === "checking,current");
    check("a check during the gateway request sends no second request", calls.gateway === 1);
    answerGateway({ outcome: "updating", version: "0.4.0" });
    await Promise.all([first, second]);
    check("both checks settle with the gateway's answer", slow.snapshot.gateway?.outcome === "updating");
    check(
      "the joined request logs once",
      slow.snapshot.log.join("|") === "PromptForge Gateway is updating to 0.4.0.",
    );
    listener.dispose();
    slow.dispose();
  }

  {
    // The gateway answers while the second check's own Workshop check runs:
    // the answer is joined, not asked for again, and its line survives the
    // check's fresh log.
    let answerGateway;
    let finishWorkshop;
    let workshopChecks = 0;
    const { service: racing, calls } = gatewayService(
      () => {
        workshopChecks += 1;
        return workshopChecks === 1 ? Promise.resolve(null) : new Promise((resolve) => (finishWorkshop = resolve));
      },
      () => new Promise((resolve) => (answerGateway = resolve)),
    );
    const first = racing.checkNow();
    for (let turn = 0; turn < 10 && answerGateway === undefined; turn += 1) {
      await Promise.resolve();
    }
    const second = racing.checkNow();
    for (let turn = 0; turn < 10 && finishWorkshop === undefined; turn += 1) {
      await Promise.resolve();
    }
    answerGateway({ outcome: "updating", version: "0.4.0" });
    await first;
    finishWorkshop(null);
    await second;
    check("a gateway answer during Workshop's check is not asked for again", calls.gateway === 1);
    check(
      "a gateway answer during Workshop's check keeps its log line",
      racing.snapshot.log.join("|") === "PromptForge Gateway is updating to 0.4.0.",
    );
    racing.dispose();
  }

  {
    // A second caller during Workshop's check also waits for the gateway.
    let answerGateway;
    const { service: joining } = gatewayService(
      async () => null,
      () => new Promise((resolve) => (answerGateway = resolve)),
    );
    let settled = 0;
    const first = joining.checkNow().then(() => (settled += 1));
    const second = joining.checkNow().then(() => (settled += 1));
    for (let turn = 0; turn < 10 && answerGateway === undefined; turn += 1) {
      await Promise.resolve();
    }
    for (let turn = 0; turn < 10; turn += 1) {
      await Promise.resolve();
    }
    check("no caller settles before the gateway answers", settled === 0);
    answerGateway({ outcome: "current" });
    await Promise.all([first, second]);
    check("every caller settles after the gateway answers", settled === 2);
    joining.dispose();
  }
});

if (failures.length) {
  console.error("Update service failures:\n" + failures.map((failure) => `  - ${failure}`).join("\n"));
  process.exitCode = 1;
} else {
  console.log("Update service checks passed.");
}
