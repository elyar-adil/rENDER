import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";

const [, , edgePath, pageUrl, outputPng, viewportWidthArg, viewportHeightArg] = process.argv;

if (!edgePath || !pageUrl || !outputPng || !viewportWidthArg || !viewportHeightArg) {
  console.error("usage: node browser_snapshot.mjs <edge-path> <url> <output.png> <width> <height>");
  process.exit(2);
}

const viewportWidth = Number.parseInt(viewportWidthArg, 10);
const viewportHeight = Number.parseInt(viewportHeightArg, 10);
const debugPort = 9333 + Math.floor(Math.random() * 2000);
const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "render-edge-"));

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function getJson(route) {
  return new Promise((resolve, reject) => {
    http.get({ host: "127.0.0.1", port: debugPort, path: route }, (res) => {
      let data = "";
      res.on("data", (chunk) => {
        data += chunk;
      });
      res.on("end", () => {
        try {
          resolve(JSON.parse(data));
        } catch (err) {
          reject(err);
        }
      });
    }).on("error", reject);
  });
}

async function main() {
  const browser = spawn(
    edgePath,
    [
      "--headless=new",
      "--disable-gpu",
      "--hide-scrollbars",
      `--window-size=${viewportWidth},${viewportHeight}`,
      `--remote-debugging-port=${debugPort}`,
      `--user-data-dir=${userDataDir}`,
      "--no-first-run",
      "--no-default-browser-check",
      pageUrl,
    ],
    { stdio: "ignore" },
  );

  try {
    let targets = null;
    for (let attempt = 0; attempt < 60; attempt += 1) {
      try {
        targets = await getJson("/json/list");
        if (Array.isArray(targets) && targets.length > 0) {
          break;
        }
      } catch {}
      await wait(500);
    }
    if (!targets || !targets.length) {
      throw new Error("failed to discover Edge debugging target");
    }

    const pageTarget = targets.find((target) => target.type === "page") || targets[0];
    const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
    let nextId = 1;
    const pending = new Map();

    ws.onmessage = (event) => {
      const message = JSON.parse(event.data);
      if (!message.id || !pending.has(message.id)) {
        return;
      }
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) {
        reject(new Error(JSON.stringify(message.error)));
      } else {
        resolve(message.result);
      }
    };

    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = reject;
    });

    const call = (method, params = {}) =>
      new Promise((resolve, reject) => {
        const id = nextId++;
        pending.set(id, { resolve, reject });
        ws.send(JSON.stringify({ id, method, params }));
      });

    await call("Page.enable");
    await call("Runtime.enable");
    await call("Page.navigate", { url: pageUrl });
    await wait(15000);

    const metrics = await call("Page.getLayoutMetrics");
    const contentSize = metrics.contentSize || { width: viewportWidth, height: viewportHeight };
    const clip = {
      x: 0,
      y: 0,
      width: Math.max(1, Math.ceil(contentSize.width || viewportWidth)),
      height: Math.max(1, Math.ceil(contentSize.height || viewportHeight)),
      scale: 1,
    };

    const screenshot = await call("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: true,
      clip,
    });
    fs.writeFileSync(outputPng, Buffer.from(screenshot.data, "base64"));

    const titleEval = await call("Runtime.evaluate", {
      expression: "document.title",
      returnByValue: true,
    });
    const title = titleEval.result?.value || "";

    ws.close();
    console.log(JSON.stringify({ title, page_height: clip.height }));
  } finally {
    browser.kill();
    try {
      fs.rmSync(userDataDir, { recursive: true, force: true });
    } catch {}
  }
}

main().catch((err) => {
  console.error(String(err && err.stack ? err.stack : err));
  process.exit(1);
});
