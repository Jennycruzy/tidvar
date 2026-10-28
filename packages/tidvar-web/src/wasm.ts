export interface Payer {
  phrase(): string;
  backup_key(): Uint8Array;
  begin(): string;
  advance(peer: string): string;
  finish_keys(peer: string, network: string): string;
  accept_seal(sealed: string, commitment: Uint8Array, round: bigint): string;
  verify_seal(openings: string): string;
  refund_transaction(info: string, beacon: string): Uint8Array;
  record_body(offer: string, terms: string): string;
  approve_record(offer: string, terms: string, partial: string): string;
  signed_record(): string;
  funding_request(): string;
  prepare_transaction(amount: bigint, cooperative: boolean): string;
  sign_transaction(prepared: string, services: string, tree: Uint8Array): string;
  prepare_request(body: string): string;
  pending_request(): string;
  accept_reply(reply: string, now: bigint): string;
  signed_amount(): bigint;
  prepare_close_request(prepared: string, services: string, tree: Uint8Array): string;
  prepare_iou_request(prepared: string, services: string, tree: Uint8Array): string;
  commitment_unused(commitment: string): boolean;
  funding_note_count(): number;
  top_up_request(amount_zat: bigint): string;
  renew_amount(used: bigint): bigint;
  prepare_renew_transaction(amount: bigint, to: string): string;
  prepare_renew_request(prepared: string, services: string, tree: Uint8Array, to: string, next_id: string): string;
  received(): string;
  payment_target(now: bigint, hidden: boolean, serviceUsed: bigint): bigint;
  payment_target_ahead(now: bigint, advanceMs: bigint, serviceUsed: bigint): bigint;
  authorization_balance(now: bigint): string;
  backup_plaintext(): Uint8Array;
  start_sync(reply: Uint8Array): void;
  apply_blocks(reply: Uint8Array): void;
  synced_height(): bigint;
  verify_tip(reply: Uint8Array): bigint;
  free(): void;
}
export interface Wasm {
  BrowserPayer: {
    new(): Payer;
    restore(phrase: string, plaintext: Uint8Array): Payer;
  };
  key_from_phrase(phrase: string): Uint8Array;
  phrase_from_entropy(entropy: Uint8Array): string;
  message_hash(message: string): string;
  new_open_id(): string;
  round_for_terms(terms: string): bigint;
  latest_request(): Uint8Array;
  info_request(): Uint8Array;
  transaction_request(txid: string): Uint8Array;
  transaction_check(reply: Uint8Array): string;
  observe_info(reply: Uint8Array): string;
  latest_height(reply: Uint8Array): bigint;
  tree_request(height: bigint): Uint8Array;
  range_request(start: bigint, end: bigint): Uint8Array;
}
let modulePath: string | undefined;
export function configureWasm(path: string): void { modulePath = path; }
export async function loadWasm(): Promise<Wasm> {
  const path = modulePath ?? new URL("../pkg/tidvar_payer.js", import.meta.url).href;
  const module = await import(path) as Wasm & {default(): Promise<unknown>};
  await module.default();
  return module;
}
