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
  private balance?: Balance;
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
    if(reply.step==="nonces"){this.pool=reply.commitments as Commitment[];this.balance=reply.balance as Balance;}
    if(reply.step==="accepted")this.balance=(reply.receipt as {balance:Balance}).balance;
    if(typeof reply.step==="string" && (reply.step.startsWith("vpn_") || reply.step.startsWith("close_") || reply.step.startsWith("chat_")) && reply.balance)this.balance=reply.balance as Balance;
    return reply;
  }
  async retry(): Promise<Record<string,unknown> | undefined> {return await this.deliver();}
  serviceBalance(): Balance | undefined {return this.balance;}
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
    payer.prepare_close_request(JSON.stringify(prepared),JSON.stringify(reply.commitments),tree);
    await this.opening.session.save(this.id);
    const closed=await this.deliver();
    if(!closed)throw new Error("The close is saved. Keep this page open and retry.");
    return closed;
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
    let refresh=height===0n || payer.funding_note_count()===0 || Date.now()-this.refreshedAt>=30*60*1000;
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
  async pay(hidden: boolean, progress: (height:bigint,value:bigint)=>void, extra = 0): Promise<PaymentEvent | undefined> {
    await this.deliver();
    const payer=this.opening.session.current();
    const balance=JSON.parse(payer.authorization_balance(BigInt(Date.now()))) as Balance;
    if(balance.paused || balance.closing || (hidden && balance.paid===0))return;
    await this.refresh(hidden,progress);
    if(!hidden)await this.nonces();
    const used=this.balance?.used ?? 0;
    if(!Number.isSafeInteger(used) || used<0)throw new Error("The service returned an invalid usage amount.");
    let amount=payer.payment_target(BigInt(Date.now()),hidden,BigInt(used));
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
