import {Opening,relay} from "./opening.js";
import {loadWasm} from "./wasm.js";
interface Commitment {id: string; data: string}
export interface Balance {paid: number; used: number; vpn_ms: number; chat_zat?: number; running: boolean; paused: boolean; closing: boolean; paid_until_ms: number | null; cut_at_ms: number | null}
interface Envelope {tab_id: string; k: number; previous: string | null; body: Record<string, unknown>}
interface Authenticated {message: Envelope; signature: string}
export interface PaymentEvent {hidden: boolean; amount: number; anchor: number; buildAndSignMs: number; deliveryMs: number; completedAt: number}
/** One chat answer, in zatoshi. */
export const CHAT_PRICE = 1000;
/** VPN time prepaid with a chat payment so the VPN stays up while the model answers (16 zat = 96 s). */
const CHAT_VPN_MARGIN = 16;
export class Tab {
  private pool: Commitment[] = [];
  private tree?: {height: bigint; bytes: Uint8Array<ArrayBuffer>};
  private refreshedAt = 0;
  private branchCheckedAt = 0;
  private balance?: Balance;
  /** Notes the service last reported counting; a top-up it has seen makes this page sync now. */
  private serviceNotes = 0;
  private prompt?: string;
  private answer: string | null = null;
  constructor(readonly opening: Opening, readonly id: string) {}
  private async deliver(): Promise<Record<string, unknown> | undefined> {
    const payer=this.opening.session.current();
    const pending=JSON.parse(payer.pending_request()) as Authenticated | null;
    if(!pending)return;
    await this.opening.session.save(this.id);
    const step=pending.message.body.step;
    let response: Response;
    const options={credentials:"omit" as const,cache:"no-store" as const,redirect:"error" as const,signal:AbortSignal.timeout(90000)};
    if(step==="nonces"){
      response=await fetch("/v1/tabs/"+this.id+"/nonces",{...options,headers:{"tidvar-request":JSON.stringify(pending)}});
    }else if(step==="iou"){
      response=await fetch("/v1/tabs/"+this.id+"/iou",{...options,method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(pending)});
    }else if(typeof step==="string" && step.startsWith("vpn_")){
      response=await fetch("/v1/tabs/"+this.id+"/vpn",{...options,method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(pending)});
    }else if(typeof step==="string" && step.startsWith("close_")){
      response=await fetch("/v1/tabs/"+this.id+"/close",{...options,method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(pending)});
    }else if(step==="charge"){
      response=await fetch("/v1/tabs/"+this.id+"/settle",{...options,method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(pending)});
    }else if(step==="chat"){
      // A chat request left pending is settled without its message: never charged.
      response=await fetch("/v1/tabs/"+this.id+"/chat",{...options,method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({request:pending,prompt:this.prompt??null})});
    }else{throw new Error("This saved request cannot be sent by the payment loop.");}
    if(!response.headers.get("content-type")?.includes("application/json"))throw new Error("The connection was interrupted. Keep this page open; the same request will be retried.");
    let text=await response.text();
    if(step==="chat" && response.ok){
      const wrapped=JSON.parse(text) as {reply: unknown; answer: string | null};
      this.answer=wrapped.answer;
      text=JSON.stringify(wrapped.reply);
    }
    if(!response.ok){
      const value=JSON.parse(text) as {error?:unknown};
      throw new Error(typeof value.error==="string"?value.error:"The service could not finish this request. Keep this page open and retry.");
    }
    const reply=JSON.parse(payer.accept_reply(text,BigInt(Date.now()))) as Record<string,unknown>;
    await this.opening.session.save(this.id);
    if(reply.step==="refused")throw new Error(typeof reply.error==="string"?reply.error:"The service refused this payment. Keep your backup and retry.");
    if(typeof reply.notes==="number")this.serviceNotes=reply.notes;
    if(reply.step==="nonces"){this.pool=reply.commitments as Commitment[];this.balance=reply.balance as Balance;}
    if(reply.step==="accepted")this.balance=(reply.receipt as {balance:Balance}).balance;
    if(typeof reply.step==="string" && (reply.step.startsWith("vpn_") || reply.step.startsWith("close_") || reply.step.startsWith("chat_") || reply.step.startsWith("charge")) && reply.balance)this.balance=reply.balance as Balance;
    return reply;
  }
  async retry(): Promise<Record<string,unknown> | undefined> {return await this.deliver();}
  serviceBalance(): Balance | undefined {return this.balance;}
  /** A payment request for more value on this tab: same address, counted after one block. */
  topUp(amountZat: number): {address: string; amount: string; amount_zat: number; uri: string} {
    return JSON.parse(this.opening.session.current().top_up_request(BigInt(amountZat))) as {address: string; amount: string; amount_zat: number; uri: string};
  }
  /** Notes this tab has received and how many the service has counted. */
  received(): {notes: number; value: number; counted: number} {
    return JSON.parse(this.opening.session.current().received()) as {notes: number; value: number; counted: number};
  }
  async vpn(body: {step: "vpn_peer"; public_key: string} | {step: "vpn_pause"} | {step: "vpn_resume"}): Promise<Record<string,unknown>> {
    await this.deliver();
    this.opening.session.current().prepare_request(JSON.stringify(body));
    await this.opening.session.save(this.id);
    const reply=await this.deliver();
    if(reply?.step==="vpn_refused")throw new Error(typeof reply.error==="string"?reply.error:"The VPN request was refused.");
    if(!reply || reply.step===undefined || !String(reply.step).startsWith("vpn_"))throw new Error("The VPN request was not confirmed. Keep this page open and retry.");
    return reply;
  }
  private async closeRequest(step: "close_start" | "close_status"): Promise<Record<string,unknown>> {
    this.opening.session.current().prepare_request(JSON.stringify({step}));
    await this.opening.session.save(this.id);
    const reply=await this.deliver();
    if(!reply || typeof reply.step!=="string" || !reply.step.startsWith("close_"))throw new Error("The close was not confirmed. Keep this page open and retry.");
    return reply;
  }
  async close(progress:(height:bigint,value:bigint)=>void):Promise<Record<string,unknown>> {
    let reply=await this.deliver();
    if(reply?.step==="close_mined" || reply?.step==="close_submitted")return reply;
    if(reply?.step!=="close_ready")reply=await this.closeRequest("close_start");
    if(reply.step!=="close_ready")return reply;
    const used=reply.used;
    if(typeof used!=="number" || !Number.isSafeInteger(used) || used<0)throw new Error("The service returned an invalid closing amount.");
    await this.opening.sync(progress);
    await this.opening.session.save(this.id);
    const payer=this.opening.session.current(),wasm=await loadWasm();
    const prepared=JSON.parse(payer.prepare_transaction(BigInt(used),true)) as {anchor_height:number};
    const tree=await relay("GetTreeState",wasm.tree_request(BigInt(prepared.anchor_height)));
    // The close spends the notes both sides have counted (the funding payment and any top-ups).
    payer.prepare_close_request(JSON.stringify(prepared),JSON.stringify((reply.commitments as Commitment[]).slice(0,payer.funding_note_count())),tree);
    await this.opening.session.save(this.id);
    const closed=await this.deliver();
    if(!closed)throw new Error("The close is saved. Keep this page open and retry.");
    return closed;
  }
  /**
   * Renews this tab: one transaction closes it and moves everything it returns into a new tab.
   * `openNext` opens the next tab for exactly that amount and returns its id and address.
   */
  async renew(openNext:(funded:number)=>Promise<{id:string;address:string}>,progress:(height:bigint,value:bigint)=>void):Promise<Record<string,unknown>> {
    let reply=await this.deliver();
    if(reply?.step==="close_mined" || reply?.step==="close_submitted")return reply;
    if(reply?.step!=="close_ready")reply=await this.closeRequest("close_start");
    if(reply.step!=="close_ready")return reply;
    const used=reply.used;
    if(typeof used!=="number" || !Number.isSafeInteger(used) || used<0)throw new Error("The service returned an invalid closing amount.");
    await this.opening.sync(progress);
    await this.opening.session.save(this.id);
    const payer=this.opening.session.current(),wasm=await loadWasm();
    const count=payer.funding_note_count();
    const funded=Number(payer.renew_amount(BigInt(used)));
    const next=await openNext(funded);
    const prepared=JSON.parse(payer.prepare_renew_transaction(BigInt(used),next.address)) as {anchor_height:number};
    const tree=await relay("GetTreeState",wasm.tree_request(BigInt(prepared.anchor_height)));
    payer.prepare_renew_request(JSON.stringify(prepared),JSON.stringify((reply.commitments as Commitment[]).slice(0,count)),tree,next.address,next.id);
    await this.opening.session.save(this.id);
    const renewed=await this.deliver();
    if(!renewed)throw new Error("The renew is saved. Keep this page open and retry.");
    return renewed;
  }
  async closeStatus():Promise<Record<string,unknown>>{
    await this.deliver();
    return await this.closeRequest("close_status");
  }
  async nonces(): Promise<Record<string,unknown>> {
    await this.deliver();
    this.opening.session.current().prepare_request(JSON.stringify({step:"nonces"}));
    await this.opening.session.save(this.id);
    const reply=await this.deliver();
    if(!reply || reply.step!=="nonces")throw new Error("The service did not return fresh signing values.");
    const usage=reply.usage as {limit?:string | null} | undefined;
    if(usage?.limit==="DATA_LIMIT")throw new Error("You've used today's data allowance. It resets at 00:00 UTC; your prepaid time is kept.");
    if(usage?.limit==="HOST_LIMIT")throw new Error("The VPN has reached this month's data limit. Your prepaid time is kept; close the tab to get it back.");
    return reply;
  }
  async refresh(hidden: boolean, progress: (height:bigint,value:bigint)=>void): Promise<void> {
    const payer=this.opening.session.current(),wasm=await loadWasm();
    let height:bigint;
    try{height=payer.synced_height();}catch{height=0n;}
    let refresh=height===0n || payer.funding_note_count()===0 || Date.now()-this.refreshedAt>=30*60*1000 || this.serviceNotes>this.received().notes;
    if(!hidden && !refresh){
      const latest=wasm.latest_height(await relay("GetLatestBlock",wasm.latest_request()));
      refresh=latest-height>=20n;
    }
    if(refresh){
      await this.opening.sync(progress);
      await this.opening.session.save(this.id);
      this.refreshedAt=Date.now();
    }
  }
  /** Pays for one chat answer (plus VPN time to cover the wait), then asks the service's model. */
  async chat(prompt: string, progress: (height:bigint,value:bigint)=>void): Promise<{answer: string; balance: Balance | undefined}> {
    const text=prompt.trim();
    if(!text || [...text].length>2000)throw new Error("Write a message of at most 2,000 characters.");
    await this.deliver();
    await this.pay(false,progress,CHAT_PRICE+CHAT_VPN_MARGIN);
    const digest=new Uint8Array(await crypto.subtle.digest("SHA-256",new TextEncoder().encode(text)));
    const hash=[...digest].map(b=>b.toString(16).padStart(2,"0")).join("");
    this.opening.session.current().prepare_request(JSON.stringify({step:"chat",prompt_sha256:hash,price:CHAT_PRICE}));
    await this.opening.session.save(this.id);
    this.prompt=text;this.answer=null;
    let reply: Record<string,unknown> | undefined;
    try{reply=await this.deliver();}finally{this.prompt=undefined;}
    const answer=this.answer;this.answer=null;
    if(reply?.step==="chat_refused")throw new Error(typeof reply.error==="string"?reply.error:"The message was not charged.");
    if(reply?.step!=="chat_answered" || typeof answer!=="string")throw new Error("The answer was not received. Keep this page open and retry.");
    return {answer,balance:this.balance};
  }
  /** Fetches a route protected by tidvar-402: pays its price from this tab, then returns the response. */
  /** Calls a tidvar-402 route (any method), paying its price from the tab, never more than `maxPrice`. */
  async fetchPaid(url: string, progress: (height:bigint,value:bigint)=>void, init: {method?: string; body?: string; headers?: Record<string,string>} = {}, maxPrice = 100000): Promise<{body: string; status: number; price: number; protocol?: string; balance: Balance | undefined}> {
    const base={credentials:"omit" as const,cache:"no-store" as const,redirect:"error" as const,method:init.method ?? "GET",body:init.body};
    const first=await fetch(url,{...base,headers:init.headers ?? {}});
    if(first.status!==402)return {body:await first.text(),status:first.status,price:0,balance:this.balance};
    const price=Number(first.headers.get("tidvar-price"));
    if(!Number.isSafeInteger(price) || price<=0 || price>Math.min(maxPrice,100000))throw new Error("This route asks "+first.headers.get("tidvar-price")+" zat, above the "+Math.min(maxPrice,100000)+" zat limit for one request.");
    await this.deliver();
    await this.pay(false,progress,price+CHAT_VPN_MARGIN);
    const payer=this.opening.session.current();
    const route=new URL(url,location.href).pathname;
    const signed=JSON.parse(payer.prepare_request(JSON.stringify({step:"charge",price,route}))) as unknown;
    await this.opening.session.save(this.id);
    // Pays over x402 v2 (PAYMENT-SIGNATURE / PAYMENT-RESPONSE) when the route offers the tidvar-tab
    // scheme there, otherwise with the Tidvar-Request header.
    const required=first.headers.get("payment-required");
    const offered=required?(JSON.parse(atob(required)) as {accepts?:{scheme?:string}[]}).accepts?.find(a=>a.scheme==="tidvar-tab"):undefined;
    const header=btoa(JSON.stringify(signed)).replace(/\+/g,"-").replace(/\//g,"_").replace(/=+$/,"");
    const payment:Record<string,string>=offered
      ? {"payment-signature":btoa(JSON.stringify({x402Version:2,accepted:offered,payload:{request:signed}}))}
      : {"tidvar-request":header};
    const paid=await fetch(url,{...base,headers:{...(init.headers ?? {}),...payment}});
    const settled=paid.headers.get("payment-response");
    const reply=offered && settled
      ? (JSON.parse(atob(settled)) as {extensions?:{tidvar?:{reply?:string}}}).extensions?.tidvar?.reply ?? null
      : paid.headers.get("tidvar-reply");
    if(!paid.ok || !reply)throw new Error((await paid.text()) || "The route was not paid.");
    const decoded=atob(reply.replace(/-/g,"+").replace(/_/g,"/"));
    const accepted=JSON.parse(payer.accept_reply(decoded,BigInt(Date.now()))) as {balance?: Balance};
    if(accepted.balance)this.balance=accepted.balance;
    await this.opening.session.save(this.id);
    return {body:await paid.text(),status:paid.status,price,protocol:offered?"x402":"tidvar",balance:this.balance};
  }
  /** How long a payment made while the page is hidden keeps the VPN paid: 1 hour unless set (at most 24). */
  awayMs = 3_600_000;
  async pay(hidden: boolean, progress: (height:bigint,value:bigint)=>void, extra = 0): Promise<PaymentEvent | undefined> {
    await this.deliver();
    const payer=this.opening.session.current();
    const balance=JSON.parse(payer.authorization_balance(BigInt(Date.now()))) as Balance;
    if(balance.paused || balance.closing || (hidden && balance.paid===0))return;
    await this.refresh(hidden,progress);
    if(!hidden)await this.nonces();
    const used=this.balance?.used ?? 0;
    if(!Number.isSafeInteger(used) || used<0)throw new Error("The service returned an invalid usage amount.");
    let amount=hidden?payer.payment_target_ahead(BigInt(Date.now()),BigInt(this.awayMs),BigInt(used)):payer.payment_target(BigInt(Date.now()),false,BigInt(used));
    if(extra>0 && BigInt(used+extra)>amount)amount=BigInt(used+extra);
    if(amount<=payer.signed_amount())return;
    const count=payer.funding_note_count();
    if(count<1 || count>4)throw new Error("Wait for your funding payment to be confirmed.");
    let available=this.pool.filter(value=>payer.commitment_unused(JSON.stringify(value)));
    if(available.length<count){
      await this.nonces();
      available=this.pool.filter(value=>payer.commitment_unused(JSON.stringify(value)));
    }
    if(available.length<count)throw new Error("Fresh signing values are not available. Keep your backup and retry.");
    if(Date.now()-this.branchCheckedAt>5*60*1000){
      // The network upgrade in force decides which transaction format the IOU uses.
      const wasm=await loadWasm();
      wasm.observe_info(await relay("GetLightdInfo",wasm.info_request()));
      this.branchCheckedAt=Date.now();
    }
    const started=performance.now();
    const prepared=JSON.parse(payer.prepare_transaction(amount,false)) as {anchor_height:number};
    const height=BigInt(prepared.anchor_height);
    if(!this.tree || this.tree.height!==height){
      const wasm=await loadWasm();
      this.tree={height,bytes:await relay("GetTreeState",wasm.tree_request(height))};
    }
    payer.prepare_iou_request(JSON.stringify(prepared),JSON.stringify(available.slice(0,count)),this.tree.bytes);
    await this.opening.session.save(this.id);
    const buildAndSignMs=performance.now()-started;
    const sent=performance.now();
    await this.deliver();
    return {hidden,amount:Number(amount),anchor:prepared.anchor_height,buildAndSignMs,deliveryMs:performance.now()-sent,completedAt:Date.now()};
  }
}
