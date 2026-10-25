export interface Payer {
  phrase(): string;
  backup_key(): Uint8Array;
  begin(): string;
  advance(peer: string): string;
  finish_keys(peer: string): string;
  accept_seal(sealed: string, commitment: Uint8Array, round: bigint): string;
  verify_seal(openings: string): string;
  record_body(offer: string, terms: string): string;
  approve_record(offer: string, terms: string, partial: string): string;
  signed_record(): string;
  backup_plaintext(): Uint8Array;
  start_sync(reply: Uint8Array): void;
  apply_blocks(reply: Uint8Array): void;
  verify_tip(reply: Uint8Array): bigint;
  free(): void;
}
export interface Wasm {
  BrowserPayer: {
    new(): Payer;
    restore(phrase: string, plaintext: Uint8Array): Payer;
  };
  key_from_phrase(phrase: string): Uint8Array;
  tree_request(height: bigint): Uint8Array;
  range_request(start: bigint, end: bigint): Uint8Array;
}
export async function loadWasm(): Promise<Wasm> {
  const path = new URL("../pkg/tidvar_payer.js", import.meta.url).href;
  const module = await import(path) as Wasm & {default(): Promise<unknown>};
  await module.default();
  return module;
}
