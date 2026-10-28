import {Session} from "./session.js";
import {loadWasm, type Wasm} from "./wasm.js";
import type {EncryptedBackup} from "./storage.js";
export interface Offer {version: number; network: "Testnet" | "Mainnet"; service_name: string; address: string; maximum_lifetime_ms: number; vpn_ms_per_zat: number; service_key: string}
export interface Terms {funded: number; open_ms: number; lifetime: "Minutes15" | "Minutes30" | "Hours2" | "Days3" | "Days7" | "Days30"; open_height: number; return_address: string}
interface Envelope {tab_id: string; k: number; previous: string | null; body: Record<string, unknown>}
interface Funding {address: string; amount_zat: number; amount: string; uri: string}
function bytes(text: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(text.replaceAll("-", "+").replaceAll("_", "/")), char => char.charCodeAt(0));
}
export function hexId(text: string): string {return Array.from(bytes(text), byte => byte.toString(16).padStart(2, "0")).join("");}
async function jsonReply(response: Response): Promise<unknown> {
  if (!response.headers.get("content-type")?.includes("application/json")) throw new Error("The connection was interrupted. Keep this page open and retry.");
  const value: unknown = await response.json();
  if (!response.ok) {
    const error = (value as {error?: unknown})?.error;
    throw new Error(typeof error === "string" ? error : "The service could not finish this step. Keep your backup and retry.");
  }
  return value;
}
export async function getOffer(): Promise<Offer> {
  return await jsonReply(await fetch("/v1/offer", {credentials: "omit", cache: "no-store", redirect: "error"})) as Offer;
}
export async function relay(method: string, body: Uint8Array): Promise<Uint8Array<ArrayBuffer>> {
  const response = await fetch("/v1/lwd", {method: "POST", credentials: "omit", cache: "no-store", redirect: "error", headers: {"content-type": "application/grpc-web+proto", "x-tidvar-method": method}, body: body.slice().buffer, signal: AbortSignal.timeout(45000)});
  if (!response.ok || !response.headers.get("content-type")?.includes("application/grpc-web")) throw new Error("We can't reach the Zcash server right now. Keep this page open and retry.");
  return new Uint8Array(await response.arrayBuffer());
}
export class Opening {
  readonly session = new Session();
  private wasm?: Wasm;
  private id?: string;
  private k = 1;
  private previous: string | null = null;
  private async exchange(body: Record<string, unknown>): Promise<Record<string, unknown>> {
    if (!this.wasm || !this.id) throw new Error("Start the tab setup first.");
    const message: Envelope = {tab_id: this.id, k: this.k, previous: this.previous, body};
    const encoded = JSON.stringify(message);
    const hash = JSON.parse(this.wasm.message_hash(encoded)) as string;
    const response = await fetch("/v1/tabs", {method: "POST", credentials: "omit", redirect: "error", cache: "no-store", headers: {"content-type": "application/json"}, body: encoded, signal: AbortSignal.timeout(90000)});
    const reply = await jsonReply(response) as Envelope;
    if (reply.tab_id !== this.id || reply.k !== this.k + 1 || reply.previous !== hash) throw new Error("The service reply does not match this setup. No money moved.");
    const replyHash = JSON.parse(this.wasm.message_hash(JSON.stringify(reply))) as string;
    this.k = reply.k + 1;
    this.previous = replyHash;
    return reply.body;
  }
  async protect(): Promise<string> {
    const phrase = await this.session.create();
    this.wasm = await loadWasm();
    this.id = JSON.parse(this.wasm.new_open_id()) as string;
    return phrase;
  }
  async open(input: {funded: number; lifetime: Terms["lifetime"]; return_address: string}, progress: (text: string) => void): Promise<{id: string; backup: EncryptedBackup; funding: Funding}> {
    const wasm = this.wasm;
    if (!wasm || !this.id) throw new Error("Protect this tab first.");
    const offer = await getOffer();
    if (offer.network !== "Testnet" && offer.network !== "Mainnet") throw new Error("This service is on an unknown network.");
    const height = wasm.latest_height(await relay("GetLatestBlock", wasm.latest_request()));
    const terms: Terms = {...input, open_ms: Date.now(), open_height: Number(height)};
    const payer = this.session.current();
    progress("Setting up the lock.");
    const first = JSON.parse(payer.begin()) as unknown;
    const keys = await this.exchange({step: "begin", offer, terms, round_one: first});
    if (keys.step !== "keys") throw new Error("The service returned the wrong setup step.");
    const second = JSON.parse(payer.advance(JSON.stringify(keys.round_one))) as unknown;
    payer.finish_keys(JSON.stringify(keys.round_two), offer.network);
    const seal = await this.exchange({step: "keys", round_two: second});
    if (seal.step !== "seal" || typeof seal.commitment !== "string") throw new Error("The service did not send its sealed key.");
    progress("Checking the service's sealed key.");
    const challenge = JSON.parse(payer.accept_seal(JSON.stringify(seal.sealed), bytes(seal.commitment), wasm.round_for_terms(JSON.stringify(terms)))) as unknown;
    const opened = await this.exchange({step: "challenge", challenge});
    if (opened.step !== "openings") throw new Error("The service did not let us check its sealed key. No money moved.");
    payer.verify_seal(JSON.stringify(opened.openings));
    progress("Checking the service's sealed key: 32 of 32 parts verified.");
    const record = JSON.parse(payer.record_body(JSON.stringify(offer), JSON.stringify(terms))) as {tab_id: string};
    const signed = await this.exchange({step: "sign", record});
    if (signed.step !== "signed") throw new Error("The service did not sign the tab terms.");
    const full = JSON.parse(payer.approve_record(JSON.stringify(offer), JSON.stringify(terms), JSON.stringify(signed.partial))) as unknown;
    const id = hexId(record.tab_id);
    const backup = await this.session.save(id);
    const accepted = await this.exchange({step: "accept", record: full});
    if (accepted.step !== "accepted" || accepted.tab_id !== record.tab_id) throw new Error("The service did not save this tab. Do not send money yet.");
    progress("The tab is saved. Keep your backup before paying.");
    return {id, backup, funding: JSON.parse(payer.funding_request()) as Funding};
  }
  async restore(phrase:string,backup:EncryptedBackup):Promise<void>{
    this.wasm=await loadWasm();
    await this.session.restore(phrase,backup);
    const record=JSON.parse(this.session.current().signed_record()) as {body:{tab_id:string}};
    if(hexId(record.body.tab_id)!==backup.id)throw new Error("The backup belongs to another tab.");
  }
  async recover(id: string): Promise<boolean> {
    this.wasm = await loadWasm();
    return await this.session.recover(id);
  }
  async sync(progress: (height: bigint, value: bigint) => void): Promise<bigint> {
    if (!this.wasm) throw new Error("Recover this tab first.");
    const wasm = this.wasm;
    const payer = this.session.current();
    const record = JSON.parse(payer.signed_record()) as {body: {terms: Terms}};
    let height: bigint;
    try {height = payer.synced_height();}
    catch {
      const start = BigInt(record.body.terms.open_height) - 1n;
      payer.start_sync(await relay("GetTreeState", wasm.tree_request(start)));
      height = start;
    }
    const tip = wasm.latest_height(await relay("GetLatestBlock", wasm.latest_request()));
    while (height < tip) {
      const end = height + 128n < tip ? height + 128n : tip;
      payer.apply_blocks(await relay("GetBlockRange", wasm.range_request(height + 1n, end)));
      height = payer.synced_height();
    }
    const value = payer.verify_tip(await relay("GetTreeState", wasm.tree_request(height)));
    progress(height, value);
    return value;
  }
}
