import "./style.css";
import {toCanvas} from "qrcode";
function element<T extends HTMLElement>(id: string): T {
  const value = document.getElementById(id);if (!value) throw new Error("The page could not be loaded.");return value as T;
}
const worker = new Worker(new URL("./worker.ts", import.meta.url), {type: "module"});
let sequence = 0;
const waiting = new Map<number, {resolve(value: unknown): void; reject(error: Error): void}>();
const progress = element("progress");const error = element("error");
worker.onmessage = event => {
  if (event.data.progress) {progress.textContent = event.data.progress; return;}
  if (event.data.chain) {
    const {height, value} = event.data.chain as {height: number; value: number};
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
async function poll(): Promise<void> {
  if (polling || paying) return;polling = true;
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
    backupUrl = URL.createObjectURL(new Blob([JSON.stringify(result.backup)], {type: "application/json"}));
    element<HTMLAnchorElement>("backup").href = backupUrl;
    element("address").textContent = funding.address;element("pay-amount").textContent = funding.amount;
    element<HTMLAnchorElement>("wallet").href = funding.uri;
    await toCanvas(element<HTMLCanvasElement>("qr"), funding.uri, {width: 280, margin: 2});
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
    const result = value as {id: string; backup: unknown; funding: typeof funding};
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
    void poll();
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
  element("meter-until").textContent = service.paused ? "Paused · prepaid time kept" : service.paid_until_ms ? new Date(service.paid_until_ms).toLocaleTimeString() : "Waiting for the first payment";
  element("vpn-pause").hidden = service.paused;
  element("vpn-resume").hidden = !service.paused;
}
async function payOnce(hidden: boolean): Promise<void> {
  try {
    const result = await call("pay", hidden) as {payment: {amount: number; buildAndSignMs: number; deliveryMs: number} | null; meter: Meter};
    showMeter(result.meter);
    if (result.payment) element("pay-status").textContent = `Paid ${result.payment.amount} zat in total. Signed in ${Math.round(result.payment.buildAndSignMs)} ms, accepted in ${Math.round(result.payment.deliveryMs)} ms.`;
  } catch (failure) {element("pay-status").textContent = failure instanceof Error ? failure.message : "The payment could not be sent. Retrying.";}
}
function schedule(): void {
  if (loop) window.clearTimeout(loop);
  loop = window.setTimeout(async () => {
    loop = undefined;
    if (document.visibilityState === "visible") await payOnce(false);
    schedule();
  }, 10000);
}
function startVpn(): void {
  if (paying) return;paying = true;
  element("vpn").hidden = false;
  void payOnce(false).then(schedule);
}
document.addEventListener("visibilitychange", () => {
  if (!paying) return;
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
    const config = `[Interface]\nPrivateKey = ${key.privateKey}\nAddress = ${reply.address}\nDNS = ${reply.dns}\n\n[Peer]\nPublicKey = ${reply.server_key}\nAllowedIPs = 0.0.0.0/0\nEndpoint = ${reply.endpoint}\nPersistentKeepalive = 25\n`;
    await toCanvas(element<HTMLCanvasElement>("vpn-qr"), config, {width: 280, margin: 2});
    if (configUrl) URL.revokeObjectURL(configUrl);
    configUrl = URL.createObjectURL(new Blob([config], {type: "text/plain"}));
    element<HTMLAnchorElement>("vpn-download").href = configUrl;
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
