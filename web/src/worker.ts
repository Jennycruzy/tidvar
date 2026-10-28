import {Opening,relay} from "../../packages/tidvar-web/src/opening.js";
import {configureWasm,loadWasm} from "../../packages/tidvar-web/src/wasm.js";
import {parseBackup} from "../../packages/tidvar-web/src/storage.js";
import {Tab} from "../../packages/tidvar-web/src/tab.js";
configureWasm("/pkg/tidvar_payer.js");
let opening = new Opening();
let tab: Tab | undefined;
let nextOpening: Opening | undefined;
// How long a payment made while the page is hidden keeps the VPN paid (set by the page).
let awayMs = 3_600_000;
const chain = (height: bigint, value: bigint) => host.postMessage({chain: {height: Number(height), value: Number(value)}});
function current(): Tab {if (!tab) throw new Error("Open or recover a tab first."); return tab;}
function meter(): unknown {
  const local = JSON.parse(opening.session.current().authorization_balance(BigInt(Date.now()))) as unknown;
  let received: unknown = null;
  try {received = current().received();} catch {received = null;}
  return {local, service: current().serviceBalance() ?? null, signed: Number(opening.session.current().signed_amount()), received};
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
    else if (type === "away") {awayMs=Math.min(86_400_000,Math.max(20_000,Number(value)||3_600_000));result=awayMs;}
    else if (type === "pay") {const tab=current();tab.awayMs=awayMs;result = {payment: await tab.pay(Boolean(value), chain) ?? null, meter: meter()};}
    else if (type === "refund") {
      // The refund needs no service: a public drand beacon opens the service's sealed share.
      await opening.sync(chain);
      const payer=opening.session.current();
      const record=JSON.parse(payer.signed_record()) as {body:{refund_ms:number;refund_round:number;tab_id:string}};
      if(Date.now()<record.body.refund_ms)throw new Error("Your refund opens at "+new Date(record.body.refund_ms).toLocaleString()+". Until then, close the tab normally to get your money back.");
      const base="https://api.drand.sh/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
      const info=await (await fetch(base+"/info",{credentials:"omit",cache:"no-store"})).text();
      const beacon=await (await fetch(base+"/public/"+record.body.refund_round,{credentials:"omit",cache:"no-store"})).text();
      result={bytes:payer.refund_transaction(info,beacon),round:record.body.refund_round};
    }
    else if (type === "paid") result = {...await current().fetchPaid(String(value), chain), meter: meter()};
    else if (type === "check_tx") {
      const wasm=await loadWasm();
      result=JSON.parse(wasm.transaction_check(await relay("GetTransaction",wasm.transaction_request(String(value))))) as unknown;
    }
    else if (type === "chat") result = {...await current().chat(String(value), chain), meter: meter()};
    else if (type === "vpn") result = {reply: await current().vpn(value as Parameters<Tab["vpn"]>[0]), meter: meter()};
    else if (type === "close") result={reply:await current().close(chain),meter:meter()};
    else if (type === "close_status") result={reply:await current().closeStatus(),meter:meter()};
    else if (type === "meter") result = meter();
    else if (type === "top_up") result = current().topUp(Number(value));
    else if (type === "renew_protect") {nextOpening = new Opening(); result = await nextOpening.protect();}
    else if (type === "renew") {
      const next = nextOpening;
      if (!next) throw new Error("Save the new tab's words first.");
      const old = current();
      const record = JSON.parse(opening.session.current().signed_record()) as {body: {terms: {return_address: string}}};
      let opened: Awaited<ReturnType<Opening["open"]>> | undefined;
      const reply = await old.renew(async funded => {
        opened = await next.open({funded, lifetime: String(value) as Parameters<Opening["open"]>[0]["lifetime"], return_address: record.body.terms.return_address}, text => host.postMessage({progress: text}));
        host.postMessage({renewing: opened.id});
        return {id: opened.id, address: opened.funding.address};
      }, chain);
      result = {reply, next: opened ? {id: opened.id, backup: opened.backup} : null, meter: meter()};
    }
    else throw new Error("This step is not available.");
    host.postMessage({id, result});
  } catch (error) {
    let snapshot: unknown;
    try {if(tab)snapshot=meter();} catch {}
    host.postMessage({id:event.data.id,meter:snapshot,error:error instanceof Error ? error.message : typeof error === "string" ? error : "This step could not finish. Keep your backup and retry."});
  } finally {busy = false;}
};
