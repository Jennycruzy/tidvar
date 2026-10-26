import {encryptBackup, decryptBackup, importBackupKey, loadBackup, saveBackup, type EncryptedBackup} from "./storage.js";
import {loadWasm, type Wasm, type Payer} from "./wasm.js";

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", {fatal: true});
interface SavedSession { phrase: string; state: number[] }
export class Session {
  private payer?: Payer;
  private wasm?: Wasm;
  private key?: CryptoKey;
  private saves: Promise<void> = Promise.resolve();
  async create(): Promise<string> {
    if (this.payer) throw new Error("A tab is already open in this page.");
    const wasm = this.wasm ??= await loadWasm();
    this.payer = new wasm.BrowserPayer();
    this.key = await importBackupKey(this.payer.backup_key());
    return this.payer.phrase();
  }
  current(): Payer {
    if (!this.payer) throw new Error("Recover your saved tab to continue.");
    return this.payer;
  }
  save(id: string): Promise<EncryptedBackup> {
    const operation = this.saves.then(() => this.saveNow(id));
    this.saves = operation.then(() => undefined, () => undefined);
    return operation;
  }
  private async saveNow(id: string): Promise<EncryptedBackup> {
    const payer = this.current();
    if (!this.key) throw new Error("The tab protection key is missing.");
    const state = payer.backup_plaintext();
    let plaintext: Uint8Array | undefined;
    try {
      plaintext = encoder.encode(JSON.stringify({phrase: payer.phrase(), state: Array.from(state)}));
      const backup = await encryptBackup(id, this.key, plaintext);
      await saveBackup(this.key, backup);
      return backup;
    } finally { state.fill(0); plaintext?.fill(0); }
  }
  private async unlock(key: CryptoKey, backup: EncryptedBackup): Promise<void> {
    if (this.payer) throw new Error("A tab is already open in this page.");
    const plaintext = await decryptBackup(key, backup);
    let state: Uint8Array | undefined;
    try {
      const saved = JSON.parse(decoder.decode(plaintext)) as SavedSession;
      if (typeof saved.phrase !== "string" || !Array.isArray(saved.state) ||
          saved.state.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
        throw new Error("The saved tab could not be read.");
      }
      state = Uint8Array.from(saved.state);
      saved.state.fill(0);
      const wasm = this.wasm ??= await loadWasm();
      const expected = wasm.key_from_phrase(saved.phrase);
      const verifier = await importBackupKey(expected);
      const checked = await decryptBackup(verifier, backup);
      checked.fill(0);
      this.payer = wasm.BrowserPayer.restore(saved.phrase, state);
      saved.phrase = "";
      this.key = key;
    } finally { plaintext.fill(0); state?.fill(0); }
  }
  async recover(id: string): Promise<boolean> {
    const saved = await loadBackup(id);
    if (!saved) return false;
    await this.unlock(saved.key, saved.backup);
    return true;
  }
  async restore(phrase: string, backup: EncryptedBackup): Promise<void> {
    const wasm = this.wasm ??= await loadWasm();
    const key = await importBackupKey(wasm.key_from_phrase(phrase));
    await this.unlock(key, backup);
    await saveBackup(key, backup);
  }
  release(): void {
    this.payer?.free();
    this.payer = undefined;
    this.key = undefined;
  }
}
