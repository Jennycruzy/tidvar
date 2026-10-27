import "./style.css";
import {toCanvas} from "qrcode";
function element<T extends HTMLElement>(id: string): T {
  const value = document.getElementById(id);if (!value) throw new Error("The page could not be loaded.");return value as T;
}
const worker = new Worker(new URL("./worker.ts", import.meta.url), {type: "module"});
let sequence = 0;
const waiting = new Map<number, {resolve(value: unknown): void; reject(error: Error): void}>();
const progress = element("progress");const error = element("error");
function observe(event:string,details:Record<string,unknown>={}):void{
  const id=localStorage.getItem("tidvar-current-tab");if(!id)return;
  const key="tidvar-timings-"+id;
  try{
    const records=JSON.parse(localStorage.getItem(key)??"[]") as unknown[];
    records.push({at:Date.now(),monotonicMs:performance.now(),visibility:document.visibilityState,event,...details});
    localStorage.setItem(key,JSON.stringify(records.slice(-5000)));
  }catch{element("timing-status").textContent="This browser could not save timing observations. Download them before leaving.";}
}

worker.onmessage = event => {
  if(event.data.observation){observe("worker",event.data.observation as Record<string,unknown>);return;}
  if(event.data.meter)showMeter(event.data.meter as Meter);
  if (event.data.progress) {progress.textContent = event.data.progress; return;}
  if (event.data.chain) {
    const {height, value} = event.data.chain as {height: number; value: number};
    observe("chain",{height,value});
    if (value > 0) startVpn();
    element("chain-status").textContent = value > 0 ? `Payment confirmed: ${decimal(BigInt(value))} TAZ. Synced through block ${height}.` : `No payment confirmed yet. Synced through block ${height}.`;
    return;
  }
  const pending = waiting.get(event.data.id);if (!pending) return;
  waiting.delete(event.data.id);
  if (event.data.error) pending.reject(new Error(event.data.error));else pending.resolve(event.data.result);
};
let queue: Promise<unknown> = Promise.resolve();
function call(type: string, value?: unknown): Promise<unknown> {
  const run = () => new Promise<unknown>((resolve, reject) => {const id = ++sequence;waiting.set(id, {resolve, reject});worker.postMessage({id, type, value});});
  const next = queue.then(run, run);
  queue = next.catch(() => undefined);
  return next;
}
function decimal(value: bigint): string {return `${value / 100000000n}.${(value % 100000000n).toString().padStart(8, "0")}`.replace(/0+$/, "").replace(/\.$/, "");}
function amount(text: string): number {
  if (!/^\d+(\.\d{1,8})?$/.test(text)) throw new Error("Enter a TAZ amount with at most eight decimal places.");
  const [whole, fraction = ""] = text.split(".");
  const value = BigInt(whole) * 100000000n + BigInt(fraction.padEnd(8, "0"));
  if (value < 50000n || value > 2100000000000000n) throw new Error("Open a tab with at least 0.0005 TAZ.");return Number(value);
}
const protect = element<HTMLButtonElement>("protect");
protect.onclick = async () => {
  protect.disabled = true;error.textContent = "";progress.textContent = "Protecting this tab on your device.";
  try {element("phrase").textContent = String(await call("protect"));element("protection").hidden = false;progress.textContent = "Save your words before continuing.";}
  catch (failure) {error.textContent = failure instanceof Error ? failure.message : "This step could not finish.";protect.disabled = false;}
};
let funding: {address: string; amount: string; uri: string} | undefined;
let backupUrl: string | undefined;
let polling = false;
let closing = localStorage.getItem("tidvar-closing-"+localStorage.getItem("tidvar-current-tab"))==="yes";
let closeTimer:number|undefined;
let stopped=localStorage.getItem("tidvar-stopped-"+localStorage.getItem("tidvar-current-tab"))==="yes";
const cutoffRequested=new URLSearchParams(location.search).get("cutoff")==="1";
let cutoffMode=cutoffRequested || localStorage.getItem("tidvar-cutoff-"+localStorage.getItem("tidvar-current-tab"))==="yes";
if(cutoffRequested){const url=new URL(location.href);url.searchParams.delete("cutoff");history.replaceState(null,"",url.pathname+url.search+url.hash);}
let closeMined=false;
function setCutoffMode(active:boolean):void{
  cutoffMode=active;
  const id=localStorage.getItem("tidvar-current-tab");
  if(id){if(active)localStorage.setItem("tidvar-cutoff-"+id,"yes");else localStorage.removeItem("tidvar-cutoff-"+id);}
  element("cutoff-status").textContent=active
    ? "Short cutoff check: background hour payments are off. Enable WireGuard, return here, then tap Stop new payments."
    : "";
}
if(cutoffMode)setCutoffMode(true);
async function poll(): Promise<void> {
  if (closing || polling || paying) return;polling = true;
  try {await call("sync");}
  catch (failure) {element("chain-status").textContent = failure instanceof Error ? failure.message : "The connection was interrupted. Retrying.";}
  finally {polling = false;setTimeout(poll, 10000);}
}
element<HTMLFormElement>("setup").onsubmit = async event => {
  event.preventDefault();error.textContent = "";
  const button = element<HTMLButtonElement>("open");button.disabled = true;
  try {
    const funded = amount(element<HTMLInputElement>("amount").value.trim());
    element("phrase").textContent = "";
    const result = await call("open", {funded, lifetime: element<HTMLSelectElement>("lifetime").value, return_address: element<HTMLInputElement>("return-address").value.trim()}) as {id: string; backup: unknown; funding: typeof funding};
    funding = result.funding;if (!funding) throw new Error("The payment request was not returned.");
    localStorage.setItem("tidvar-current-tab", result.id);
    if(cutoffMode)setCutoffMode(true);
    void sampleClock();
    backupUrl = URL.createObjectURL(new Blob([JSON.stringify(result.backup)], {type: "application/json"}));
    element<HTMLAnchorElement>("backup").href = backupUrl;
    element("address").textContent = funding.address;element("pay-amount").textContent = funding.amount;
    element<HTMLAnchorElement>("wallet").href = funding.uri;
    await toCanvas(element<HTMLCanvasElement>("qr"), funding.uri, {width: 280, margin: 2});
    element("restore-tab").hidden=true;
    element("setup").hidden = true;element("funding").hidden = false;
    history.replaceState(null, "", "/tab");void poll();
  } catch (failure) {error.textContent = failure instanceof Error ? failure.message : "The setup could not finish. No money moved.";button.disabled = false;}
};
for (const [id, field] of [["copy-address", "address"], ["copy-amount", "amount"]] as const) {
  element<HTMLButtonElement>(id).onclick = async () => {
    if (!funding) return;
    try {await navigator.clipboard.writeText(funding[field]);progress.textContent = field === "address" ? "Address copied." : "Amount copied.";}
    catch {error.textContent = "Copy failed. Select and copy the text shown above.";}
  };
}
const stored = localStorage.getItem("tidvar-current-tab");
if (stored) {
  protect.disabled = true;
  progress.textContent = "Recovering your saved tab.";
  void call("recover", stored).then(async value => {
    const result = value as {id: string; backup: unknown; funding: typeof funding;closing:boolean};
    if(result.closing){closing=true;localStorage.setItem("tidvar-closing-"+result.id,"yes");}
    element("restore-tab").hidden=true;
    funding = result.funding;
    if (!funding) throw new Error("The saved payment request could not be read.");
    backupUrl = URL.createObjectURL(new Blob([JSON.stringify(result.backup)], {type:"application/json"}));
    element<HTMLAnchorElement>("backup").href = backupUrl;
    element("address").textContent = funding.address;
    element("pay-amount").textContent = funding.amount;
    element<HTMLAnchorElement>("wallet").href = funding.uri;
    await toCanvas(element<HTMLCanvasElement>("qr"), funding.uri, {width:280,margin:2});
    element("setup").hidden=true;element("funding").hidden=false;
    progress.textContent = "Your saved tab is recovered.";
    if(closing){element("vpn").hidden=false;void closeTab();}else void poll();
  }).catch(failure => {error.textContent=failure instanceof Error ? failure.message : "The saved tab could not be recovered.";});
}
window.addEventListener("pagehide", () => {if (backupUrl) URL.revokeObjectURL(backupUrl);});

interface Balance {paid: number; used: number; running: boolean; paused: boolean; closing: boolean; paid_until_ms: number | null}
interface Meter {local: Balance; service: Balance | null; signed: number}
let paying = false;
let loop: number | undefined;
function showMeter(meter: Meter): void {
  const service = meter.service ?? meter.local;
  element("meter-paid").textContent = `${meter.signed} zat`;
  element("meter-used").textContent = `${service.used} zat`;
  element("meter-until").textContent = closing ? (service.closing ? "Closing · billing stopped" : "Closing · no new payments") : service.paused ? "Paused · prepaid time kept" : service.paid_until_ms ? new Date(service.paid_until_ms).toLocaleTimeString() : "Waiting for the first payment";
  element("prepare-cutoff").hidden=closing;
  element("stop-payments").hidden=closing;
  if(closing)element("restart-payments").hidden=true;
  element("vpn-pause").hidden = closing || service.paused;
  element("vpn-resume").hidden = closing || !service.paused;
}
async function payOnce(hidden: boolean): Promise<void> {
  observe("payment_requested",{hidden});
  try {
    const result = await call("pay", hidden) as {payment: {amount: number; buildAndSignMs: number; deliveryMs: number} | null; meter: Meter};
    showMeter(result.meter);
    if(result.payment)observe("payment_accepted",{...result.payment,hidden});
    if(result.payment && hidden)awayPaid=true;
    if (result.payment) element("pay-status").textContent = `Paid ${result.payment.amount} zat in total. Signed in ${Math.round(result.payment.buildAndSignMs)} ms, accepted in ${Math.round(result.payment.deliveryMs)} ms.`;
  } catch (failure) {element("pay-status").textContent = failure instanceof Error ? failure.message : "The payment could not be sent. Retrying.";}
}
let awayPaid = false;
function schedule(): void {
  if(closing || stopped)return;
  if (loop) window.clearTimeout(loop);
  loop = window.setTimeout(async () => {
    loop = undefined;
    observe("timer");
    if (!stopped && document.visibilityState === "visible") await payOnce(false);
    // A hidden page whose visibilitychange was missed still pays its away time, once per hide.
    else if (!stopped && !cutoffMode && !awayPaid) await payOnce(true);
    schedule();
  }, 10000);
}
function startVpn(): void {
  if (closing) return;
  element("vpn").hidden = false;
  if(stopped){element("restart-payments").hidden=false;return;}
  if(paying)return;paying = true;
  void payOnce(false).then(schedule);
}
document.addEventListener("visibilitychange", () => {
  observe("visibility");
  if (document.visibilityState === "visible") awayPaid = false;
  if (closing || stopped || !paying) return;
  if (document.visibilityState === "hidden" && cutoffMode){observe("cutoff_away_skipped");return;}
  if (document.visibilityState === "hidden") void payOnce(true);
  else void payOnce(false).then(schedule);
});
function base64(bytes: Uint8Array): string {return btoa(String.fromCharCode(...bytes));}
async function wireguardKey(id: string): Promise<{privateKey: string; publicKey: string}> {
  const name = `tidvar-wg-${id}`;
  let saved: string | null = null;
  try {saved = localStorage.getItem(name);} catch {saved = null;}
  if (saved) return JSON.parse(saved) as {privateKey: string; publicKey: string};
  let pair: CryptoKeyPair;
  try {pair = await crypto.subtle.generateKey({name: "X25519"}, true, ["deriveBits"]) as CryptoKeyPair;}
  catch {throw new Error("This browser cannot create a WireGuard key. Use a current Chrome, Firefox or Safari.");}
  const privateKey = base64(new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey)).slice(-32));
  const publicKey = base64(new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey)));
  const value = {privateKey, publicKey};
  try {localStorage.setItem(name, JSON.stringify(value));} catch {throw new Error("This browser cannot save the VPN key. Turn off private browsing and retry.");}
  return value;
}
let configUrl: string | undefined;
element<HTMLButtonElement>("vpn-setup").onclick = async () => {
  const button = element<HTMLButtonElement>("vpn-setup");button.disabled = true;error.textContent = "";
  try {
    const id = localStorage.getItem("tidvar-current-tab");if (!id) throw new Error("Open or recover a tab first.");
    const key = await wireguardKey(id);
    const result = await call("vpn", {step: "vpn_peer", public_key: key.publicKey}) as {reply: {address: string; dns: string; server_key: string; endpoint: string}; meter: Meter};
    const reply = result.reply;
    const config = `[Interface]\nPrivateKey = ${key.privateKey}\nAddress = ${reply.address}\nDNS = ${reply.dns}\n\n[Peer]\nPublicKey = ${reply.server_key}\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = ${reply.endpoint}\nPersistentKeepalive = 25\n`;
    await toCanvas(element<HTMLCanvasElement>("vpn-qr"), config, {width: 280, margin: 2});
    if (configUrl) URL.revokeObjectURL(configUrl);
    configUrl = URL.createObjectURL(new Blob([config], {type: "application/octet-stream"}));
    element<HTMLAnchorElement>("vpn-download").href = configUrl;
    element<HTMLAnchorElement>("vpn-download").download = "tidvar-"+id.slice(0,6)+".conf";
    element("vpn-config").hidden = false;
    showMeter(result.meter);
  } catch (failure) {error.textContent = failure instanceof Error ? failure.message : "The VPN could not be set up.";}
  finally {button.disabled = false;}
};
for (const [id, step] of [["vpn-pause", "vpn_pause"], ["vpn-resume", "vpn_resume"]] as const) {
  element<HTMLButtonElement>(id).onclick = async () => {
    error.textContent = "";
    try {showMeter((await call("vpn", {step}) as {meter: Meter}).meter);}
    catch (failure) {error.textContent = failure instanceof Error ? failure.message : "The request could not finish. Retry.";}
  };
}
window.addEventListener("pagehide", () => {if (configUrl) URL.revokeObjectURL(configUrl);});

interface CloseResult {step:string;error?:string;close?:{txid:string;amount:number;fee:number;returned:number;height:number}}
function showClose(reply:CloseResult):void{
  const status=element("close-status");
  observe("close_status",{step:reply.step,...reply.close});
  if(reply.step==="close_retry"){
    status.textContent=reply.error ?? "The close needs a refresh. Click Close to retry.";
    element<HTMLButtonElement>("close-tab").disabled=false;
    return;
  }
  if(reply.close){
    const receipt=reply.close;
    status.textContent=reply.step==="close_mined"
      ? "Closed in block "+receipt.height+". Returned "+decimal(BigInt(receipt.returned))+" TAZ; service "+receipt.amount+" zat; network fee "+receipt.fee+" zat. Transaction: "+receipt.txid
      : "Close submitted. Waiting for a block. Transaction: "+receipt.txid;
    element("funding").hidden=true;
    element("vpn-config").hidden=true;
    element("vpn-setup").hidden=true;
  }
  if(reply.step==="close_mined"){
    closeMined=true;
    element("meter-chain").textContent="2 transactions";
    element("new-tab").hidden=false;
    element<HTMLButtonElement>("close-tab").disabled=true;
    return;
  }
  if(reply.step==="close_expired"){
    status.textContent="The close expired without being mined. Retrying with a fresh transaction.";
    closeTimer=window.setTimeout(()=>void closeTab(),5000);
  }else{
    closeTimer=window.setTimeout(()=>void checkClose(),5000);
  }
}
async function checkClose():Promise<void>{
  try{
    const result=await call("close_status") as {reply:CloseResult;meter:Meter};
    showMeter(result.meter);showClose(result.reply);
  }catch(failure){
    element("close-status").textContent=failure instanceof Error?failure.message:"The close status could not be read. Retrying.";
    closeTimer=window.setTimeout(()=>void checkClose(),5000);
  }
}
async function closeTab():Promise<void>{
  closing=true;
  observe("close_requested");
  if(loop)window.clearTimeout(loop);
  if(closeTimer)window.clearTimeout(closeTimer);
  const id=localStorage.getItem("tidvar-current-tab");
  if(id)localStorage.setItem("tidvar-closing-"+id,"yes");
  const button=element<HTMLButtonElement>("close-tab");
  button.disabled=true;
  element("vpn-pause").hidden=true;element("vpn-resume").hidden=true;
  element("close-status").textContent="Stopping paid time and preparing your return transaction.";
  try{
    const result=await call("close") as {reply:CloseResult;meter:Meter};
    showMeter(result.meter);showClose(result.reply);
  }catch(failure){
    element("close-status").textContent=failure instanceof Error?failure.message:"The close is saved. Retry to continue.";
    button.disabled=false;
  }
}
element<HTMLButtonElement>("close-tab").onclick=()=>void closeTab();


element<HTMLButtonElement>("download-timings").onclick=()=>{
  const id=localStorage.getItem("tidvar-current-tab");if(!id)return;
  const data=localStorage.getItem("tidvar-timings-"+id)??"[]";
  const url=URL.createObjectURL(new Blob([data],{type:"application/json"}));
  const link=document.createElement("a");link.href=url;link.download="tidvar-timings.json";link.click();
  window.setTimeout(()=>URL.revokeObjectURL(url),1000);
};
observe("page_loaded");

async function downloadBackup():Promise<void>{
  try{
    const backup=await call("backup");
    const url=URL.createObjectURL(new Blob([JSON.stringify(backup)],{type:"application/json"}));
    const link=document.createElement("a");link.href=url;link.download="tidvar-backup.json";link.click();
    window.setTimeout(()=>URL.revokeObjectURL(url),1000);
    observe("backup_downloaded");
  }catch(failure){error.textContent=failure instanceof Error?failure.message:"The latest backup could not be saved.";}
}
element<HTMLAnchorElement>("backup").onclick=event=>{event.preventDefault();void downloadBackup();};
element<HTMLButtonElement>("export-backup").onclick=()=>void downloadBackup();
element<HTMLFormElement>("restore-tab").onsubmit=async event=>{
  event.preventDefault();error.textContent="";
  const file=element<HTMLInputElement>("restore-file").files?.[0];
  const phrase=element<HTMLInputElement>("restore-phrase").value.trim();
  element<HTMLInputElement>("restore-phrase").value="";
  if(!file || file.size>32*1024*1024){error.textContent="Choose your encrypted Tidvar backup file.";return;}
  const button=element<HTMLButtonElement>("restore-submit");button.disabled=true;
  try{
    const result=await call("restore",{phrase,backup:await file.text()}) as {id:string;closing:boolean};
    localStorage.setItem("tidvar-current-tab",result.id);
    if(result.closing)localStorage.setItem("tidvar-closing-"+result.id,"yes");
    history.replaceState(null,"","/tab");location.reload();
  }catch(failure){error.textContent=failure instanceof Error?failure.message:"The backup could not be recovered.";button.disabled=false;}
};

let monitoring=false;
async function monitor():Promise<void>{
  if(!monitoring)return;
  const started=Date.now();
  try{
    const response=await fetch("/v1/offer",{credentials:"omit",cache:"no-store",redirect:"error",signal:AbortSignal.timeout(cutoffMode?1000:2500)});
    if(!response.ok)throw new Error("Unreachable");
    element("connection-status").textContent="Connection reachable at "+new Date().toLocaleTimeString()+".";
    observe("connection",{ok:true,started,durationMs:Date.now()-started});
  }catch{
    element("connection-status").textContent="Connection lost at "+new Date().toLocaleTimeString()+". Turn WireGuard off to reconnect.";
    observe("connection",{ok:false,started,durationMs:Date.now()-started});
  }
  if(monitoring)window.setTimeout(()=>void monitor(),cutoffMode?250:1000);
}
element<HTMLButtonElement>("stop-payments").onclick=async()=>{
  stopped=true;
  if(loop)window.clearTimeout(loop);
  const id=localStorage.getItem("tidvar-current-tab");if(id)localStorage.setItem("tidvar-stopped-"+id,"yes");
  await queue;
  observe("payments_stopped");
  element("pay-status").textContent="New payments stopped. The VPN cuts when your paid time runs out. Away time already paid still counts; Pause cuts immediately.";
  element("restart-payments").hidden=false;
  if(!monitoring){monitoring=true;void monitor();}
};
element<HTMLButtonElement>("restart-payments").onclick=()=>{
  setCutoffMode(false);
  stopped=false;
  const id=localStorage.getItem("tidvar-current-tab");if(id)localStorage.removeItem("tidvar-stopped-"+id);
  observe("payments_restarted");
  element("restart-payments").hidden=true;
  void payOnce(false).then(schedule);
};

// A visible UTC clock lets a phone recording locate the wallet send against server observations.
function recordingClock():void{element("recording-clock").textContent="UTC "+new Date().toISOString();}
recordingClock();window.setInterval(recordingClock,100);
async function sampleClock():Promise<void>{
  const started=Date.now();
  try{
    const response=await fetch("/v1/offer",{credentials:"omit",cache:"no-store",signal:AbortSignal.timeout(10000)});
    const serverDate=response.headers.get("date");
    if(serverDate)observe("clock_sample",{started,finished:Date.now(),serverDate});
  }catch{}
}
void sampleClock();

element<HTMLButtonElement>("prepare-cutoff").onclick=async()=>{
  if(closing)return;
  const button=element<HTMLButtonElement>("prepare-cutoff");button.disabled=true;
  const previous={stopped,paying,cutoffMode};
  error.textContent="";setCutoffMode(true);
  element("cutoff-status").textContent="Preparing the short cutoff check. Keep this page visible.";
  try{
    stopped=false;
    const id=localStorage.getItem("tidvar-current-tab");if(id)localStorage.removeItem("tidvar-stopped-"+id);
    paying=true;
    await payOnce(false);
    const snapshot=await call("meter") as Meter;
    const balance=snapshot.service;
    if(!balance || balance.paused || balance.closing)throw new Error("Resume the VPN before starting the cutoff check.");
    const credit=balance.paid-balance.used;
    if(credit<=0 || credit>4)throw new Error("This tab already has away time prepaid, or needs a new foreground payment. Existing credit is kept.");
    observe("cutoff_prepared",{paidUntil:balance.paid_until_ms});
    setCutoffMode(true);showMeter(snapshot);schedule();
  }catch(failure){
    stopped=previous.stopped;paying=previous.paying;setCutoffMode(previous.cutoffMode);
    const id=localStorage.getItem("tidvar-current-tab");
    if(id){if(stopped)localStorage.setItem("tidvar-stopped-"+id,"yes");else localStorage.removeItem("tidvar-stopped-"+id);}
    if(paying && !stopped)schedule();
    error.textContent=failure instanceof Error?failure.message:"The cutoff check could not start.";
  }finally{button.disabled=false;}
};
element<HTMLButtonElement>("new-tab").onclick=()=>{
  if(!closeMined)return;
  localStorage.removeItem("tidvar-current-tab");
  location.assign("/open");
};
