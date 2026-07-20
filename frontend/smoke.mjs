#!/usr/bin/env node
// frontend/smoke.mjs — headless Playwright smoke against the REAL local stack.
//
// Serves this directory over http (config.json overridden by the emitted
// frontend-config.json) and asserts, against the live validator left up by
// `scripts/solana_e2e_local.sh --keep-alive` in
// BreadchainCoop/commonware-restaking:
//
//   (a) the gk_state PDA decodes for real: transition_count == 1 and a 64-hex
//       commitment_root, rendered in the On-chain history panel,
//   (b) the settled story text (from the real buffer account, offset 0) renders
//       with the in-browser sha256 verification badge showing VERIFIED,
//   (c) the contracts panel shows the real program ids + state PDA,
//   (d) zero console errors and zero page errors.
//
// Usage:
//   node smoke.mjs <path/to/frontend-config.json> [--screenshot out.png]
//   node smoke.mjs --unconfigured [--screenshot out.png]
//       (serves the shipped empty config.json and asserts the honest
//        "network not deployed yet" state renders error-free instead)
//
// Env: SMOKE_CONFIG (alternative to the positional config path),
//      SMOKE_STORY  (expected story substring; default below).
//
// Requires: `playwright` resolvable from the cwd or from this directory
// (npm i playwright && npx playwright install chromium).

import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const FRONTEND_DIR = fileURLToPath(new URL(".", import.meta.url));
const EXPECT_STORY =
  process.env.SMOKE_STORY || "there was a little girl named Lily";

function die(msg) {
  console.error("SMOKE FAIL: " + msg);
  process.exit(1);
}

// ---- args ----
const args = process.argv.slice(2);
let configPath = process.env.SMOKE_CONFIG || null;
let screenshot = null;
let unconfigured = false;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--screenshot") screenshot = args[++i];
  else if (args[i] === "--unconfigured") unconfigured = true;
  else if (!args[i].startsWith("--")) configPath = args[i];
  else die("unknown flag " + args[i]);
}
if (!unconfigured && !configPath)
  die(
    "no config: node smoke.mjs <frontend-config.json> " +
      "(emitted by solana_e2e_local.sh into .solana-e2e/out/), or --unconfigured",
  );

let cfg = null;
if (!unconfigured) {
  if (!existsSync(configPath)) die("config not found: " + configPath);
  cfg = JSON.parse(readFileSync(configPath, "utf8"));
  for (const k of ["rpcUrl", "ncnProgramId", "settlementProgramId", "statePda"])
    if (!cfg[k]) die("config missing field: " + k);
}

// ---- playwright (resolve from cwd first, then from this directory) ----
let chromium = null;
for (const base of [join(process.cwd(), "x"), join(FRONTEND_DIR, "x")]) {
  try {
    ({ chromium } = createRequire(base)("playwright"));
    break;
  } catch {
    /* try next base */
  }
}
if (!chromium)
  die(
    "playwright not resolvable — npm i playwright && npx playwright install chromium",
  );

// ---- static server for the frontend dir, /config.json overridden ----
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".json": "application/json",
  ".png": "image/png",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
};
const server = createServer((req, res) => {
  let path = new URL(req.url, "http://x").pathname;
  if (path === "/") path = "/index.html";
  if (path === "/config.json" && !unconfigured) {
    res.writeHead(200, { "Content-Type": MIME[".json"] });
    res.end(readFileSync(configPath));
    return;
  }
  const file = normalize(join(FRONTEND_DIR, path.slice(1)));
  if (!file.startsWith(FRONTEND_DIR) || !existsSync(file)) {
    res.writeHead(404);
    res.end("not found");
    return;
  }
  res.writeHead(200, {
    "Content-Type": MIME[extname(file)] || "application/octet-stream",
  });
  res.end(readFileSync(file));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const origin = `http://127.0.0.1:${server.address().port}`;
console.log(`smoke: serving ${FRONTEND_DIR} at ${origin}`);

// ---- drive ----
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 1400 } });
const consoleErrors = [];
const pageErrors = [];
page.on("console", (m) => {
  if (m.type() === "error") consoleErrors.push(m.text());
});
page.on("pageerror", (e) => pageErrors.push(String(e)));
page.on("requestfailed", (r) =>
  console.warn(`smoke: request failed ${r.url()} (${r.failure()?.errorText})`),
);

let failures = 0;
const assert = (cond, label) => {
  if (cond) console.log("  PASS  " + label);
  else {
    console.error("  FAIL  " + label);
    failures++;
  }
};

try {
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });

  if (unconfigured) {
    await page.waitForFunction(
      () => /network not deployed yet/i.test(document.body.innerText),
      { timeout: 30_000 },
    );
    assert(
      (await page.textContent("#llm-pill")).includes("NOT DEPLOYED"),
      "pill shows NOT DEPLOYED",
    );
    assert(
      /network not deployed yet/i.test(await page.textContent("#history")),
      "history shows the honest unconfigured state",
    );
  } else {
    // (a)+(b): history renders a decoded story entry from the live chain
    await page.waitForSelector("#history .story", { timeout: 60_000 });
    const st = await page.evaluate(() => chainState);
    assert(st !== null, "gk_state PDA decoded (177-byte layout, disc 0x60)");
    assert(
      st.transition_count === 1,
      `transition_count == 1 (got ${st && st.transition_count})`,
    );
    assert(
      /^[0-9a-f]{64}$/.test(st.commitment_root),
      `commitment_root is 64-hex (${st && st.commitment_root})`,
    );
    const history = await page.textContent("#history");
    assert(
      history.includes(st.commitment_root.slice(0, 16)),
      "commitment_root hex shown in On-chain history",
    );
    assert(/transitions\s*1/.test(history), "history shows transitions 1");
    assert(
      history.includes(EXPECT_STORY),
      `story text renders ("${EXPECT_STORY}")`,
    );
    assert(
      (await page.locator("#history .story .m .ok").first().textContent())
        .includes("verified"),
      "story sha256 badge shows VERIFIED",
    );

    // exercise the chat read path too: Read the chain -> bot message
    await page.waitForSelector("#send:not([disabled])", { timeout: 15_000 });
    await page.click("#send");
    await page.waitForSelector(".msg.bot", { timeout: 30_000 });
    const bot = await page.textContent(".msg.bot");
    assert(bot.includes(EXPECT_STORY), "chat panel reads the settled story");
    assert(bot.includes("verified ✓"), "chat panel story sha256 verified");

    // (c): contracts panel shows the real ids
    const contracts = await page.textContent("#contracts");
    assert(
      contracts.includes(cfg.ncnProgramId),
      "contracts panel shows the NCN program id",
    );
    assert(
      contracts.includes(cfg.settlementProgramId),
      "contracts panel shows the settlement program id",
    );
    assert(
      contracts.includes(cfg.statePda),
      "contracts panel shows the state PDA",
    );
  }

  // give late async work (watch tick, buffer fetches) a beat to surface errors
  await page.waitForTimeout(2_000);

  // (d): zero console/page errors
  assert(
    consoleErrors.length === 0,
    "zero console errors" +
      (consoleErrors.length ? " — " + consoleErrors.join(" | ") : ""),
  );
  assert(
    pageErrors.length === 0,
    "zero page errors" +
      (pageErrors.length ? " — " + pageErrors.join(" | ") : ""),
  );

  if (screenshot) {
    await page.screenshot({ path: screenshot, fullPage: true });
    console.log("smoke: screenshot -> " + screenshot);
  }
} catch (e) {
  failures++;
  console.error("  FAIL  " + (e.stack || e));
} finally {
  await browser.close();
  server.close();
}

if (failures) die(failures + " assertion(s) failed");
console.log(
  "SMOKE PASS" + (unconfigured ? " (unconfigured state)" : " (live story)"),
);
