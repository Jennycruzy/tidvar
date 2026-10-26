import {Opening,relay} from "./opening.js";
import {loadWasm} from "./wasm.js";
interface Commitment {id: string; data: string}
export interface Balance {paid: number; used: number; vpn_ms: number; running: boolean; paused: boolean; closing: boolean; paid_until_ms: number | null; cut_at_ms: number | null}
interface Envelope {tab_id: string; k: number; previous: string | null; body: Record<string, unknown>}
interface Authenticated {message: Envelope; signature: string}
export interface PaymentEvent {hidden: boolean; amount: number; anchor: number; buildAndSignMs: number; deliveryMs: number; completedAt: number}
export class Tab {
  private pool: Commitment[] = [];
  private tree?: {height: bigint; bytes: Uint8Array<ArrayBuffer>};
  private refreshedAt = 0;
  private balance?: Balance;
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
    }else{throw new Error("This saved request cannot be sent by the payment loop.");}
    if(!response.headers.get("content-type")?.includes("application/json"))throw new Error("The connection was interrupted. Keep this page open; the same request will be retried.");
    const text=await response.text();
    if(!response.ok){
      const value=JSON.parse(text) as {error?:unknown};
      throw new Error(typeof value.error==="string"?value.error:"The service could not finish this request. Keep this page open and retry.");
    }
    const reply=JSON.parse(payer.accept_reply(text,BigInt(Date.now()))) as Record<string,unknown>;
    await this.opening.session.save(this.id);
    if(reply.step==="refused")throw new Error(typeof reply.error==="string"?reply.error:"The service refused this payment. Keep your backup and retry.");
    if(reply.step==="nonces"){this.pool=reply.commitments as Commitment[];this.balance=reply.balance as Balance;}
    if(reply.step==="accepted")this.balance=(reply.receipt as {balance:Balance}).balance;
    if(typeof reply.step==="string" && reply.step.startsWith("vpn_") && reply.balance)this.balance=reply.balance as Balance;
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
  async pay(hidden: boolean, progress: (height:bigint,value:bigint)=>void): Promise<PaymentEvent | undefined> {
    await this.deliver();
    const payer=this.opening.session.current();
    const balance=JSON.parse(payer.authorization_balance(BigInt(Date.now()))) as Balance;
    if(balance.paused || balance.closing || (hidden && balance.paid===0))return;
    await this.refresh(hidden,progress);
    if(!hidden)await this.nonces();
    const used=this.balance?.used ?? 0;
    if(!Number.isSafeInteger(used) || used<0)throw new Error("The service returned an invalid usage amount.");
    const amount=payer.payment_target(BigInt(Date.now()),hidden,BigInt(used));
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
