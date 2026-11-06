import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";

const browser = process.env.AMATERASU_BROWSER ?? (process.platform === "win32"
  ? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
  : process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "chromium");
let browserAvailable = false;
try {
  execFileSync(browser, ["--version"], { stdio: "ignore" });
  browserAvailable = true;
} catch {}

test("real Chromium dashboard qualification (provider APIs are mocked)", {
  skip: !browserAvailable && `Browser executable unavailable: ${browser}`,
}, async () => {
  const sourceFiles = [
    "src/locale-catalogs.ts",
    "src/locale.ts",
    "src/model.ts",
    "src/ui.ts",
    "src/provider_refresh.ts",
    "src/integrations.ts",
  ];
  const compiled = ts.transpile(
    sourceFiles.map((file) => readFileSync(file, "utf8")).join("\n"),
    { target: ts.ScriptTarget.ES2022 },
  );
  const browserQualification = `
    globalThis.renderRtxHdrIntegration = globalThis.Amaterasu.renderRtxHdrIntegration = () => {};
    const main = document.querySelector("main");
    const lutrisStatus = {
      ok: true, settings_ready: true, enabled: true, platform_supported: true,
      database_available: true, executable_available: true, available: true,
      game_count: 1234, refresh_pending: false, last_error: null,
    };
    const playniteStatus = {
      ok: true, settings_ready: true, enabled: true, playnite_running: true,
      snapshot_pending: false, snapshot_game_count: 27, last_error: null,
    };
    const calls = [];
    let playniteStatusReads = 0;
    let playniteRefreshAccepted = false;
    let playniteReadbackFailed = false;
    let lutrisRefreshCalls = 0;
    let finishPendingLutrisRefresh;
    const api = {
      allows: () => true,
      async post(route) {
        calls.push(route);
        if (route === "steam/status")
          return { ok:true, settings_ready:true, enabled:true, game_count:1234,
            warning_count:4, scanning:false, complete:true };
        if (route === "lutris/status") return lutrisStatus;
        if (route === "playnite/status") {
          playniteStatusReads += 1;
          if (playniteRefreshAccepted && !playniteReadbackFailed) {
            playniteReadbackFailed = true;
            throw new Error("mocked Playnite readback outage");
          }
          if (playniteReadbackFailed)
            return { ...playniteStatus, snapshot_pending:true };
          return playniteStatus;
        }
        if (route === "lutris/refresh") {
          lutrisRefreshCalls += 1;
          if (lutrisRefreshCalls === 2)
            return new Promise((resolve) => { finishPendingLutrisRefresh = resolve; });
          return { ...lutrisStatus, game_count:1234, refresh_pending:false };
        }
        if (route === "playnite/refresh") {
          playniteRefreshAccepted = true;
          return { ok:true };
        }
        throw new Error("unexpected mocked provider route: " + route);
      },
    };
    const sleep = (milliseconds = 0) => new Promise((resolve) => setTimeout(resolve, milliseconds));
    const waitFor = async (predicate, label) => {
      for (let attempt = 0; attempt < 100; attempt += 1) {
        if (predicate()) return;
        await sleep(10);
      }
      throw new Error("Timed out waiting for " + label);
    };
    const card = (provider) => [...document.querySelectorAll(".integration-card")]
      .find((item) => item.querySelector("h2")?.textContent === provider);
    const actionButton = (provider) => [...card(provider).querySelectorAll("button")]
      .find((item) => item.getAttribute("data-locale-aria-label") === "Request a {provider} refresh");
    const statusFeedback = (provider) => card(provider).querySelector(".integration-action-status");
    const statusRefresh = () => [...document.querySelectorAll("button")]
      .find((item) => item.textContent === "Refresh provider statuses");

    (async () => {
      const checks = [];
      try {
        Amaterasu.renderIntegrations({ content: main, navigate: () => {}, api });
        await waitFor(() => actionButton("Lutris") && !actionButton("Lutris").disabled, "initial provider status");

        const lutrisButton = actionButton("Lutris");
        const playniteButton = actionButton("Playnite");
        if (lutrisButton.getAttribute("aria-label") !== "Request a Lutris refresh" ||
            playniteButton.getAttribute("aria-label") !== "Request a Playnite refresh")
          throw new Error("provider refresh buttons lack distinct accessible names");
        checks.push("provider-specific accessible names");

        const englishCount = card("Steam").querySelector(".integration-status").textContent;
        Amaterasu.applyLocale("fr");
        const frenchCount = card("Steam").querySelector(".integration-status").textContent;
        if (frenchCount === englishCount ||
            !frenchCount.includes("Jeux détectés : " + new Intl.NumberFormat("fr-FR").format(1234)))
          throw new Error("count template did not update when the locale changed");
        if (actionButton("Lutris").getAttribute("aria-label") !== "Demander l’actualisation de Lutris")
          throw new Error("provider button accessible name did not follow the locale");
        if (!card("Lutris").querySelector("p").textContent.includes("Détecter les jeux"))
          throw new Error("provider description did not follow the locale");
        Amaterasu.applyLocale("en");
        checks.push("live locale switch and localized count templates");

        const initialRefreshCalls = calls.filter((route) => route.endsWith("/refresh")).length;
        statusRefresh().click();
        await waitFor(
          () => statusFeedback("Lutris").textContent.includes("no provider refresh was requested.") &&
            statusFeedback("Playnite").textContent.includes("no provider refresh was requested."),
          "read-only status feedback",
        );
        if (calls.filter((route) => route.endsWith("/refresh")).length !== initialRefreshCalls)
          throw new Error("read-only status refresh called a provider refresh route");
        checks.push("truthful read-only status refresh");

        const refresh = actionButton("Lutris");
        refresh.focus();
        refresh.click();
        await waitFor(
          () => statusFeedback("Lutris").textContent.includes("current provider status was read back."),
          "accepted Lutris refresh readback",
        );
        if (document.activeElement !== refresh)
          throw new Error("provider refresh moved keyboard focus");
        checks.push("accepted refresh readback and focus retention");

        actionButton("Playnite").click();
        await waitFor(
          () => statusFeedback("Playnite").textContent.includes("status readback failed."),
          "accepted Playnite refresh with failed status readback",
        );
        if (!statusFeedback("Playnite").textContent.includes("mocked Playnite readback outage") ||
            !actionButton("Playnite").disabled)
          throw new Error("accepted refresh result or retry warning was lost after readback failure");
        checks.push("accepted refresh with readback failure");

        statusRefresh().click();
        await waitFor(
          () => statusFeedback("Playnite").textContent.includes("no provider refresh was requested.") &&
            playniteStatusReads >= 3,
          "later read-only status check",
        );
        if (statusFeedback("Playnite").textContent.includes("Refresh accepted"))
          throw new Error("read-only status check falsely claimed a provider refresh");
        checks.push("read-only follow-up after partial failure");

        const pendingButton = actionButton("Lutris");
        const pendingFeedback = statusFeedback("Lutris");
        pendingButton.click();
        await waitFor(() => lutrisRefreshCalls === 2 && typeof finishPendingLutrisRefresh === "function",
          "second pending Lutris refresh");
        if (!pendingButton.disabled) throw new Error("pending refresh remained actionable");
        pendingButton.click();
        if (lutrisRefreshCalls !== 2) throw new Error("concurrent click duplicated a pending refresh");
        main.remove();
        finishPendingLutrisRefresh({ ...lutrisStatus, refresh_pending:false, game_count:8 });
        await sleep(30);
        if (pendingFeedback.textContent !== "Refreshing provider...")
          throw new Error("a detached dashboard was updated after navigation");
        checks.push("focus-safe navigation cleanup during pending refresh");

        document.body.dataset.browserChecks = String(checks.length);
        document.body.dataset.browserQualification = "passed";
        const result = document.createElement("pre");
        result.id = "qualification-result";
        result.textContent = JSON.stringify(checks);
        document.body.append(result);
      } catch (error) {
        document.body.dataset.browserQualification = "failed";
        const result = document.createElement("pre");
        result.id = "qualification-result";
        result.textContent = error instanceof Error ? error.stack : String(error);
        document.body.append(result);
      }
    })();
  `;
  const html = `<!doctype html>
    <html lang="en"><head><meta charset="utf-8">
      <link rel="stylesheet" href="${pathToFileURL(resolve("public/styles.css"))}">
      <title>Integration dashboard browser qualification</title>
    </head><body><main></main>
      <script>window.addEventListener("error", event => document.body.dataset.browserError = event.message);
      ${compiled}\n${browserQualification}</script>
    </body></html>`;
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "amaterasu-dashboard-browser-"));
  const htmlPath = join(temporaryDirectory, "qualification.html");
  writeFileSync(htmlPath, html);
  try {
    const output = execFileSync(browser, [
      "--headless",
      "--no-sandbox",
      "--disable-gpu",
      "--no-first-run",
      "--disable-extensions",
      "--allow-file-access-from-files",
      "--virtual-time-budget=10000",
      "--dump-dom",
      pathToFileURL(htmlPath).href,
    ], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    assert.match(output, /data-browser-qualification="passed"/, output.slice(-6000));
    assert.match(output, /data-browser-checks="7"/);
    assert.match(output, /<pre id="qualification-result">.*focus-safe navigation cleanup during pending refresh.*<\/pre>/s);
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
});
