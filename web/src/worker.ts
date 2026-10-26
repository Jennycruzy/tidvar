import {Opening} from "../../packages/tidvar-web/src/opening.js";
import {configureWasm} from "../../packages/tidvar-web/src/wasm.js";
import {Tab} from "../../packages/tidvar-web/src/tab.js";
configureWasm("/pkg/tidvar_payer.js");
const opening = new Opening();
let tab: Tab | undefined;
const chain = (height: bigint, value: bigint) => host.postMessage({chain: {height: Number(height), value: Number(value)}});
function current(): Tab {if (!tab) throw new Error("Open or recover a tab first."); return tab;}
function meter(): unknown {
  const local = JSON.parse(opening.session.current().authorization_balance(BigInt(Date.now()))) as unknown;
  return {local, service: current().serviceBalance() ?? null, signed: Number(opening.session.current().signed_amount())};
}
const host = globalThis as unknown as {onmessage: ((event: MessageEvent) => void) | null; postMessage(message: unknown): void};
let busy = false;
host.onmessage = async event => {
  if (busy) {host.postMessage({id: event.data.id, error: "This step is still running. Keep the page open."}); return;}
  busy = true;
  try {
    const {id, type, value} = event.data as {id: number; type: string; value: unknown};
    let result: unknown;
    if (type === "protect") result = await opening.protect();
    else if (type === "open") {
      const opened = await opening.open(value as Parameters<Opening["open"]>[0], text => host.postMessage({progress: text}));
      tab = new Tab(opening, opened.id);
      result = opened;
    }
    else if (type === "recover") {
      const id=String(value);
      if (!await opening.recover(id)) throw new Error("This tab was not found. Use your downloaded backup.");
      tab = new Tab(opening, id);
      result={id,backup:await opening.session.save(id),funding:JSON.parse(opening.session.current().funding_request())};
    }
    else if (type === "sync") result = await opening.sync(chain);
    else if (type === "pay") result = {payment: await current().pay(Boolean(value), chain) ?? null, meter: meter()};
    else if (type === "vpn") result = {reply: await current().vpn(value as Parameters<Tab["vpn"]>[0]), meter: meter()};
    else if (type === "meter") result = meter();
    else throw new Error("This step is not available.");
    host.postMessage({id, result});
  } catch (error) {
    host.postMessage({id: event.data.id, error: error instanceof Error ? error.message : "This step could not finish. Keep your backup and retry."});
  } finally {busy = false;}
};
