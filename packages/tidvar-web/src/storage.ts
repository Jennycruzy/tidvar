const encoder = new TextEncoder();
const MAX_BYTES = 32 * 1024 * 1024;
export interface EncryptedBackup {
  version: 1;
  id: string;
  iv: string;
  ciphertext: string;
}

function encode(bytes: Uint8Array): string {
  let text = "";
  for (let offset = 0; offset < bytes.length; offset += 8192) {
    text += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  }
  return btoa(text).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
}
function decode(text: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]*$/.test(text) || text.length % 4 === 1) {
    throw new Error("The backup contains invalid data.");
  }
  const value = atob(text.replaceAll("-", "+").replaceAll("_", "/"));
  const bytes = Uint8Array.from(value, char => char.charCodeAt(0));
  if (encode(bytes) !== text) throw new Error("The backup contains invalid data.");
  return bytes;
}
function checkId(id: string): void {
  if (!/^[a-f0-9]{64}$/.test(id)) throw new Error("The saved tab has an invalid identifier.");
}
function authenticatedData(id: string): Uint8Array<ArrayBuffer> {
  checkId(id);
  return encoder.encode(`TidvarBackup1:${id}`);
}
export function parseBackup(text: string): EncryptedBackup {
  if (text.length > MAX_BYTES) throw new Error("The backup is too large.");
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object") throw new Error("The backup could not be read.");
  const backup = value as Record<string, unknown>;
  if (Object.keys(backup).sort().join(",") !== "ciphertext,id,iv,version" ||
      backup.version !== 1 || typeof backup.id !== "string" ||
      typeof backup.iv !== "string" || typeof backup.ciphertext !== "string") {
    throw new Error("The backup format is not supported.");
  }
  checkId(backup.id);
  if (decode(backup.iv).length !== 12 || decode(backup.ciphertext).length < 16) {
    throw new Error("The backup contains invalid data.");
  }
  return backup as unknown as EncryptedBackup;
}
export async function importBackupKey(raw: Uint8Array): Promise<CryptoKey> {
  try {
    if (raw.length !== 32) throw new Error("The backup key has the wrong length.");
    return await crypto.subtle.importKey("raw", raw.slice().buffer, "AES-GCM", false, ["encrypt", "decrypt"]);
  } finally {
    raw.fill(0);
  }
}
export async function encryptBackup(id: string, key: CryptoKey, plaintext: Uint8Array): Promise<EncryptedBackup> {
  try {
    if (plaintext.length > MAX_BYTES / 2) throw new Error("The saved tab is too large.");
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ciphertext = await crypto.subtle.encrypt({name: "AES-GCM", iv, additionalData: authenticatedData(id)}, key, plaintext.slice().buffer);
    return {version: 1, id, iv: encode(iv), ciphertext: encode(new Uint8Array(ciphertext))};
  } finally {
    plaintext.fill(0);
  }
}
export async function decryptBackup(key: CryptoKey, input: EncryptedBackup): Promise<Uint8Array<ArrayBuffer>> {
  const backup = parseBackup(JSON.stringify(input));
  try {
    return new Uint8Array(await crypto.subtle.decrypt({name: "AES-GCM", iv: decode(backup.iv), additionalData: authenticatedData(backup.id)}, key, decode(backup.ciphertext)));
  } catch {
    throw new Error("The backup could not be unlocked. Check your phrase and keep the original file.");
  }
}
function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("tidvar", 1);
    request.onupgradeneeded = () => {
      request.result.createObjectStore("keys");
      request.result.createObjectStore("tabs");
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error("Your browser could not open saved tabs."));
    request.onblocked = () => reject(new Error("Close other Tidvar pages before recovering this tab."));
  });
}
export async function saveBackup(key: CryptoKey, backup: EncryptedBackup): Promise<void> {
  const checked = parseBackup(JSON.stringify(backup));
  if (key.extractable || key.algorithm.name !== "AES-GCM" || !key.usages.includes("decrypt")) {
    throw new Error("The backup key must stay protected on this device.");
  }
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      const transaction = db.transaction(["keys", "tabs"], "readwrite", {durability: "strict"});
      transaction.objectStore("keys").put(key, checked.id);
      transaction.objectStore("tabs").put(checked, checked.id);
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(new Error("Your tab could not be saved. Keep this page open and download your backup."));
      transaction.onerror = () => reject(new Error("Your tab could not be saved. Keep this page open and download your backup."));
    });
  } finally { db.close(); }
}
export async function loadBackup(id: string): Promise<{key: CryptoKey; backup: EncryptedBackup} | null> {
  checkId(id);
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const transaction = db.transaction(["keys", "tabs"], "readonly");
      const key = transaction.objectStore("keys").get(id);
      const tab = transaction.objectStore("tabs").get(id);
      transaction.oncomplete = () => {
        if (key.result === undefined && tab.result === undefined) { resolve(null); return; }
        if (!(key.result instanceof CryptoKey) || tab.result === undefined) {
          reject(new Error("The saved tab is incomplete. Recover it with your downloaded backup.")); return;
        }
        try { resolve({key: key.result, backup: parseBackup(JSON.stringify(tab.result))}); }
        catch (error) { reject(error); }
      };
      transaction.onabort = () => reject(new Error("Your saved tab could not be read."));
      transaction.onerror = () => reject(new Error("Your saved tab could not be read."));
    });
  } finally { db.close(); }
}
