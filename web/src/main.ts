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
    element("chain-status").textContent = value > 0 ? `Payment confirmed: ${decimal(BigInt(value))} TAZ. Synced through block ${height}.` : `No payment confirmed yet. Synced through block ${height}.`;
    return;
  }
  const pending = waiting.get(event.data.id);if (!pending) return;
  waiting.delete(event.data.id);
  if (event.data.error) pending.reject(new Error(event.data.error));else pending.resolve(event.data.result);
};
function call(type: string, value?: unknown): Promise<unknown> {
  const id = ++sequence;
  return new Promise((resolve, reject) => {waiting.set(id, {resolve, reject});worker.postMessage({id, type, value});});
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
  if (polling) return;polling = true;
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
