// Two-tab browser P2P acceptance for the uniblox transport demo.
//
// Prereqs:
//   1. cargo run -p services            (signaling on :3536)
//   2. ./scripts/build-wasm.sh          (build dist/)
//   3. ./scripts/serve.sh               (static server on :8080, no COOP/COEP)
//   4. npm i playwright && npx playwright install chromium   (any dir)
//   5. node scripts/e2e-two-tab.mjs
//
// Opens two headless-chromium pages, collects their consoles, and requires each
// page to receive BOTH a state-channel and an events-channel packet from the
// other peer. Exit 0 = PASS.
//
// KNOWN ENVIRONMENT LIMITATION (2026-07, WSL2): headless Chrome under WSL2
// never fires ICE-gathering-state 'complete' when ANY iceServers entry is
// configured (candidates ARE gathered; the state machine just never
// completes; reproduced on both chromium-headless-shell and full chromium,
// with and without --allow-loopback-in-peer-connection). matchbox_socket
// 0.14's wasm handshake waits for gathering-complete before sending its
// offer (non-trickle; a matchbox TODO), so the handshake stalls and this
// test TIMES OUT under WSL2 headless. It passes wherever gathering
// completes normally — a desktop browser or a non-WSL CI host. Manual
// verification: run prereqs 1-3 and open http://localhost:8080/ in two
// real browser tabs; each console must log [STATE] and [EVENT] receipts.
import { chromium } from "playwright";

const URL = "http://localhost:8080/";
const TIMEOUT_MS = 30_000;

// Flags for automated WebRTC: unhide host-candidate IPs (mDNS names don't
// resolve headlessly) and allow the loopback interface as an ICE candidate.
const browser = await chromium.launch({
  headless: true,
  args: [
    "--disable-features=WebRtcHideLocalIpsWithMdns",
    "--allow-loopback-in-peer-connection",
  ],
});
const context = await browser.newContext();

function watch(page, tag, store) {
  page.on("console", (msg) => {
    const text = msg.text();
    store.push(text);
    if (text.includes("[uniblox-demo]")) console.log(`${tag} ${text}`);
  });
  page.on("pageerror", (err) => console.log(`${tag} PAGEERROR ${err.message}`));
}

const logsA = [];
const logsB = [];
const pageA = await context.newPage();
watch(pageA, "[tabA]", logsA);
await pageA.goto(URL);
const pageB = await context.newPage();
watch(pageB, "[tabB]", logsB);
await pageB.goto(URL);

const gotBoth = (logs) =>
  logs.some((l) => l.includes("[uniblox-demo][STATE] from")) &&
  logs.some((l) => l.includes("[uniblox-demo][EVENT] from"));

const deadline = Date.now() + TIMEOUT_MS;
let pass = false;
while (Date.now() < deadline) {
  if (gotBoth(logsA) && gotBoth(logsB)) {
    pass = true;
    break;
  }
  await new Promise((r) => setTimeout(r, 250));
}

await browser.close();

if (pass) {
  console.log("PASS: both tabs received packets on BOTH channels (state + events)");
  process.exit(0);
} else {
  console.log("FAIL: markers not seen in both tabs within timeout");
  console.log("---- tabA console (full) ----");
  for (const l of logsA) console.log(`[tabA] ${l}`);
  console.log("---- tabB console (full) ----");
  for (const l of logsB) console.log(`[tabB] ${l}`);
  process.exit(1);
}
