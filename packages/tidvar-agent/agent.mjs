#!/usr/bin/env node
// tidvar-agent: an AI agent's wallet is one Tidvar tab. The tab's size is the agent's hard
// spending limit, enforced by the 2-of-2 lock, not by this program. Commands:
//   tidvar-agent open --service <https://…> --amount 0.0005 --return <your address>
//   tidvar-agent status | ask "<question>" | fetch <url> | close
//   tidvar-agent mcp            (MCP stdio server: tab_status, ask, fetch_paid, close_tab)
import {readFileSync, writeFileSync, mkdirSync, existsSync, chmodSync} from "node:fs";
import {homedir} from "node:os";
import {join, dirname} from "node:path";
import {fileURLToPath, pathToFileURL} from "node:url";
import {webcrypto} from "node:crypto";

const here = dirname(fileURLToPath(import.meta.url));
const kit = join(here, "..", "tidvar-web");
const home = process.env.TIDVAR_AGENT_HOME ?? join(homedir(), ".tidvar-agent");
const statePath = join(home, "tab.json");
mkdirSync(home, {recursive: true, mode: 0o700});

// ---- what a browser would provide: the service origin, saved tabs, WebCrypto
function loadState() { return existsSync(statePath) ? JSON.parse(readFileSync(statePath, "utf8")) : {}; }
function saveState(state) { writeFileSync(statePath, JSON.stringify(state, null, 2), {mode: 0o600}); chmodSync(statePath, 0o600); }
let state = loadState();
function installBrowserShims(origin) {
  const base = new URL(origin);
  if (base.protocol !== "https:" && base.hostname !== "127.0.0.1") throw new Error("Use the service's https:// address.");
  globalThis.location = base;
  if (!globalThis.crypto) globalThis.crypto = webcrypto;
  const realFetch = globalThis.fetch;
  globalThis.fetch = (input, init) => realFetch(typeof input === "string" ? new URL(input, base) : input, init);
  // The kit keeps each tab's encrypted backup and its non-extractable key in IndexedDB. Here the
  // key lives in memory and the encrypted backup is written to ~/.tidvar-agent/tab.json.
  const stores = {keys: new Map(), tabs: new Map()};
  const db = {
    close() {},
    transaction() {
      const tx = {};
      const pending = [];
      tx.objectStore = name => ({
        put(value, key) {
          stores[name].set(key, value);
          if (name === "tabs") { state.backup = value; saveState(state); }
          return {};
        },
        get(key) { const request = {}; pending.push(() => { request.result = stores[name].get(key); }); return request; },
      });
      setTimeout(() => { pending.forEach(run => run()); tx.oncomplete?.(); }, 0);
      return tx;
    },
  };
  globalThis.indexedDB = {open() {
    const request = {result: db};
    setTimeout(() => { request.onsuccess?.(); }, 0);
    return request;
  }};
}

async function kitModules() {
  const wasmPath = join(kit, "pkg", "tidvar_payer_bg.wasm");
  const glue = await import(pathToFileURL(join(kit, "pkg", "tidvar_payer.js")).href);
  await glue.default({module_or_path: readFileSync(wasmPath)});
  const wasm = await import(pathToFileURL(join(kit, "dist", "wasm.js")).href);
  // Point the kit at the already initialised module.
  const wrapper = join(home, "wasm-ready.mjs");
  writeFileSync(wrapper, `export * from ${JSON.stringify(pathToFileURL(join(kit, "pkg", "tidvar_payer.js")).href)};\nexport default async function(){}\n`);
  wasm.configureWasm(pathToFileURL(wrapper).href);
  const {Opening} = await import(pathToFileURL(join(kit, "dist", "opening.js")).href);
  const {Tab} = await import(pathToFileURL(join(kit, "dist", "tab.js")).href);
  return {Opening, Tab};
}
const chain = () => {};

async function openTab(args) {
  const service = option(args, "--service"), amount = option(args, "--amount") ?? "0.0005", returnAddress = option(args, "--return");
  if (!service || !returnAddress) throw new Error("open needs --service <https://…> and --return <your shielded address>.");
  if (state.id) throw new Error("This agent already has a tab. Close it first.");
  installBrowserShims(service);
  const {Opening, Tab} = await kitModules();
  const opening = new Opening();
  const phrase = await opening.protect();
  state = {service, phrase};
  saveState(state);
  const [whole, fraction = ""] = amount.split(".");
  const funded = Number(BigInt(whole) * 100000000n + BigInt(fraction.padEnd(8, "0").slice(0, 8)));
  const opened = await opening.open({funded, lifetime: "Days3", return_address: returnAddress}, text => console.error(text));
  state.id = opened.id;
  saveState(state);
  const funding = opened.funding;
  console.log(JSON.stringify({tab: opened.id, pay: funding}, null, 2));
  console.error("Pay this from your wallet. The agent can spend at most this tab, and gets the rest back to you on close.");
  return new Tab(opening, opened.id);
}
async function currentTab() {
  if (!state.id) throw new Error("This agent has no tab yet. Run: tidvar-agent open --service … --amount … --return …");
  installBrowserShims(state.service);
  const {Opening, Tab} = await kitModules();
  const opening = new Opening();
  await opening.restore(state.phrase, state.backup);
  const tab = new Tab(opening, state.id);
  await tab.retry();
  return tab;
}
async function status(tab) {
  const payer = tab.opening.session.current();
  const local = JSON.parse(payer.authorization_balance(BigInt(Date.now())));
  const synced = await tab.opening.sync(chain);
  return {tab: state.id, service: state.service, funded_zat: JSON.parse(payer.signed_record()).body.terms.funded, synced_through: Number(synced), signed_zat: Number(payer.signed_amount()), local, service_balance: tab.serviceBalance() ?? null};
}
function option(args, name) { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; }

// The most one request may cost (zat), set by the person who starts the agent, not by the model.
const MAX_PRICE = Number(process.env.TIDVAR_MAX_PRICE ?? 5000);
async function paidCall(tab, url, method, body, contentType) {
  const init = {method: method ? String(method).toUpperCase() : (body ? "POST" : "GET")};
  if (body !== undefined) { init.body = String(body); init.headers = {"content-type": contentType ? String(contentType) : "application/json"}; }
  return await tab.fetchPaid(url, chain, init, MAX_PRICE);
}

// ---- MCP stdio server
async function mcp() {
  let tab;
  const tools = [
    {name: "tab_status", description: "How much of the agent's Tidvar tab is paid, used and left. The tab size is the hard spending limit.", inputSchema: {type: "object", properties: {}}},
    {name: "ask", description: "Ask Tidvar Chat (a private model on the service's server). Costs 1,000 zat from the tab.", inputSchema: {type: "object", properties: {question: {type: "string"}}, required: ["question"]}},
    {name: "fetch_paid", description: "Call a URL protected by tidvar-402 (any HTTP API behind it, e.g. an OpenAI-compatible model), paying its price from the tab. Refuses a price above the per-request limit and anything beyond the tab.", inputSchema: {type: "object", properties: {url: {type: "string"}, method: {type: "string", description: "GET (default) or POST"}, body: {type: "string", description: "Request body, for example JSON"}, content_type: {type: "string", description: "Defaults to application/json when a body is given"}}, required: ["url"]}},
    {name: "close_tab", description: "Close the tab: pays the service what was used and returns the rest to the owner's address.", inputSchema: {type: "object", properties: {}}},
  ];
  const run = async (name, input) => {
    tab ??= await currentTab();
    if (name === "tab_status") return await status(tab);
    if (name === "ask") return await tab.chat(String(input.question ?? ""), chain);
    if (name === "fetch_paid") return await paidCall(tab, String(input.url ?? ""), input.method, input.body, input.content_type);
    if (name === "close_tab") return await tab.close(chain);
    throw new Error("Unknown tool.");
  };
  let buffer = "";
  const reply = message => process.stdout.write(JSON.stringify({jsonrpc: "2.0", ...message}) + "\n");
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", async chunk => {
    buffer += chunk;
    let line;
    while ((line = buffer.indexOf("\n")) >= 0) {
      const text = buffer.slice(0, line).trim(); buffer = buffer.slice(line + 1);
      if (!text) continue;
      const request = JSON.parse(text);
      if (request.id === undefined) continue;
      try {
        if (request.method === "initialize") reply({id: request.id, result: {protocolVersion: "2024-11-05", capabilities: {tools: {}}, serverInfo: {name: "tidvar-agent", version: "0.1.0"}}});
        else if (request.method === "tools/list") reply({id: request.id, result: {tools}});
        else if (request.method === "tools/call") {
          try { const value = await run(request.params.name, request.params.arguments ?? {}); reply({id: request.id, result: {content: [{type: "text", text: JSON.stringify(value, (k, v) => typeof v === "bigint" ? Number(v) : v)}]}}); }
          catch (error) { reply({id: request.id, result: {isError: true, content: [{type: "text", text: error instanceof Error ? error.message : String(error)}]}}); }
        } else reply({id: request.id, error: {code: -32601, message: "Method not found"}});
      } catch (error) { reply({id: request.id, error: {code: -32603, message: String(error)}}); }
    }
  });
}

const [command, ...args] = process.argv.slice(2);
const print = value => console.log(JSON.stringify(value, (k, v) => typeof v === "bigint" ? Number(v) : v, 2));
try {
  if (command === "open") await openTab(args);
  else if (command === "status") print(await status(await currentTab()));
  else if (command === "ask") print(await (await currentTab()).chat(args.join(" "), chain));
  else if (command === "fetch") print(await paidCall(await currentTab(), args[0], option(args, "--method"), option(args, "--data")));
  else if (command === "close") print(await (await currentTab()).close(chain));
  else if (command === "mcp") await mcp();
  else { console.error("usage: tidvar-agent open --service <https://…> --amount 0.0005 --return <address> | status | ask <question> | fetch <url> [--data <body>] [--method POST] | close | mcp"); process.exitCode = 2; }
} catch (error) { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; }
