// Click-through of the ltc-mtc page in headless Chromium: save as, edit + Ctrl+S, start/stop,
// export/import cues, the load dialog. Run it against a demo (scripts/ui-demo.sh start with a
// fresh LTC_TEST_DIR holding a copy of the demo config): it saves a show "Demo Zürich".
//
// usage: PLAYWRIGHT_CORE=<path to node_modules/playwright-core> [CHROMIUM=<executable>] \
//        node scripts/ui-e2e.js OUT_DIR http://<pi>:8088/
const { chromium } = require(process.env.PLAYWRIGHT_CORE || "playwright-core");
const fs = require("fs");
const OUT = process.argv[2], URL = process.argv[3];
const exe = process.env.CHROMIUM || undefined;
const ok = (cond, msg) => { console.log((cond ? "PASS " : "FAIL ") + msg); if (!cond) process.exitCode = 1; };
(async () => {
  const browser = await chromium.launch({ executablePath: exe, args: ["--no-sandbox", "--no-proxy-server"] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, acceptDownloads: true });
  const dialogs = [];
  page.on("dialog", (d) => { dialogs.push(d.message()); d.accept(); });
  await page.goto(URL);
  await page.waitForFunction(() => document.querySelector("#tc").textContent.includes(":"));
  await page.waitForTimeout(800);
  ok((await page.textContent("#showname")) === "(not saved as a show)", "fresh device: no show");
  ok((await page.textContent("#arm")).includes("ACTIVE"), "cues active at start");

  // Save as
  await page.click("#saveas");
  await page.fill("dialog input.name", "Probe/1");
  ok(await page.isDisabled("dialog button.primary"), "a name with / is refused");
  await page.fill("dialog input.name", "Demo Zürich");
  await page.click("dialog button.primary");
  await page.waitForFunction(() => document.querySelector("#showname").textContent === "Demo Zürich");
  ok(true, "saved as 'Demo Zürich'");

  // Edit, then Save (Ctrl+S)
  const before = await page.locator(".ev").count();
  await page.click("#add");
  ok(await page.isVisible("#dirty"), "an edit shows 'unsaved changes'");
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => !document.querySelector("#dirty").classList.contains("on"));
  const cfg = await (await page.request.get(URL + "api/config")).json();
  ok(cfg.events.length === before + 1, `Ctrl+S applied the new cue (${cfg.events.length} cues)`);
  const shows = await (await page.request.get(URL + "api/shows")).json();
  ok(shows.shows.find((s) => s.name === "Demo Zürich").events === before + 1, "and wrote it to the show file");

  // Start / stop
  await page.click("#arm");
  await page.waitForFunction(() => document.querySelector("#arm").textContent.includes("STOPPED"));
  ok((await (await page.request.get(URL + "api/status")).json()).armed === false, "STOP reaches the device");
  await page.waitForTimeout(400);
  await page.screenshot({ path: OUT + "/stopped.png" });
  await page.click("#arm");
  await page.waitForFunction(() => document.querySelector("#arm").textContent.includes("ACTIVE"));
  ok((await (await page.request.get(URL + "api/status")).json()).armed === true, "START reaches the device");

  // Export cues
  const [dl] = await Promise.all([page.waitForEvent("download"), page.click("#export")]);
  const set = JSON.parse(fs.readFileSync(await dl.path(), "utf8"));
  ok(set.format === "ltc-mtc cue set" && set.events.length === before + 1, `export: ${set.events.length} cues, file "${dl.suggestedFilename()}"`);
  ok(Array.isArray(set.osc_targets) && set.osc_targets.every((t) => set.events.some((e) => e.commands.some((c) => c.target === t.name))),
    `export carries only the OSC targets in use: ${set.osc_targets.map((t) => t.name).join(", ")}`);

  // Import a cue set that needs a new OSC target, adding to the list
  const imp = { format: "ltc-mtc cue set", version: 1,
    osc_targets: [{ name: "Lights", host: "10.0.0.9", port: 8000, protocol: "udp" }, { name: "QLab", host: "1.2.3.4", port: 1, protocol: "udp" }],
    events: [{ name: "imported", time: "01:02:03:04", commands: [{ type: "osc", target: "Lights", address: "/go", args: [] }] }] };
  fs.writeFileSync(OUT + "/set.json", JSON.stringify(imp));
  dialogs.length = 0;
  page.once("dialog", () => {}); // the replace/add question: accept() above answers OK = replace
  await page.setInputFiles("#importfile", OUT + "/set.json");
  await page.waitForTimeout(300);
  const targets = await page.$$eval(".target input:first-child", (els) => els.map((e) => e.value));
  ok(targets.includes("Lights"), `import added the missing target (targets: ${targets.join(", ")})`);
  const qlab = await page.evaluate(() => cfg.osc_targets.find((t) => t.name === "QLab"));
  ok(qlab && qlab.host === "127.0.0.1", "an existing target of the same name was kept");
  await page.click("#revert"); // throw the import away
  await page.waitForTimeout(300);

  // Load dialog
  await page.click("#load");
  await page.waitForSelector("dialog .dlg-row");
  await page.waitForTimeout(200);
  await page.screenshot({ path: OUT + "/load.png" });
  ok((await page.locator("dialog .dlg-row.cur").textContent()).includes("Demo Zürich"), "load dialog marks the current show");
  await page.click("dialog .dlg-foot button:last-child");
  await browser.close();
})().catch((e) => { console.log("FAIL " + e.message); process.exit(1); });
