import {Opening} from "../../packages/tidvar-web/src/opening.js";
import {configureWasm,loadWasm} from "../../packages/tidvar-web/src/wasm.js";
import {parseBackup} from "../../packages/tidvar-web/src/storage.js";
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
      result={id,backup:await opening.session.save(id),funding:JSON.parse(opening.session.current().funding_request()),closing:JSON.parse(opening.session.current().authorization_balance(BigInt(Date.now()))).closing};
    }
    else if(type==="restore" || type==="judge"){
      const given=value as {phrase?:string;entropy?:number[];backup:string};
      const saved={phrase:given.phrase ?? (await loadWasm()).phrase_from_entropy(Uint8Array.from(given.entropy ?? [])),backup:given.backup};
      const backup=parseBackup(saved.backup);
      await opening.restore(saved.phrase,backup);
      tab=new Tab(opening,backup.id);
      result={id:backup.id,closing:JSON.parse(opening.session.current().authorization_balance(BigInt(Date.now()))).closing};
    }
    else if(type==="backup")result=await opening.session.save(current().id);
    else if (type === "sync") {
      const started=performance.now();
      result=await opening.sync(chain);
      const memory=(performance as Performance & {memory?:{usedJSHeapSize:number}}).memory?.usedJSHeapSize ?? null;
      const wasm=performance.getEntriesByType("resource").filter(entry=>entry.name.includes("_bg.wasm")).at(-1) as PerformanceResourceTiming|undefined;
      host.postMessage({observation:{step:"sync",elapsedMs:performance.now()-started,heapBytes:memory,wasmBytes:wasm?.decodedBodySize ?? null}});
    }
    else if (type === "pay") result = {payment: await current().pay(Boolean(value), chain) ?? null, meter: meter()};
    else if (type === "chat") result = {...await current().chat(String(value), chain), meter: meter()};
    else if (type === "vpn") result = {reply: await current().vpn(value as Parameters<Tab["vpn"]>[0]), meter: meter()};
    else if (type === "close") result={reply:await current().close(chain),meter:meter()};
    else if (type === "close_status") result={reply:await current().closeStatus(),meter:meter()};
    else if (type === "meter") result = meter();
    else throw new Error("This step is not available.");
    host.postMessage({id, result});
  } catch (error) {
    let snapshot: unknown;
    try {if(tab)snapshot=meter();} catch {}
    host.postMessage({id:event.data.id,meter:snapshot,error:error instanceof Error ? error.message : "This step could not finish. Keep your backup and retry."});
  } finally {busy = false;}
};
