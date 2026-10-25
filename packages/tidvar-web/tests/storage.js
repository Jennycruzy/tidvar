import {importBackupKey, encryptBackup, decryptBackup, saveBackup, loadBackup} from "../dist/storage.js";

export async function checkStorage() {
  const id = Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, "0")).join("");
  const raw = crypto.getRandomValues(new Uint8Array(32));
  const key = await importBackupKey(raw);
  if (raw.some(byte => byte !== 0) || key.extractable) throw new Error("The backup key was not protected.");
  const plaintext = crypto.getRandomValues(new Uint8Array(256));
  const expected = plaintext.slice();
  const backup = await encryptBackup(id, key, plaintext);
  if (plaintext.some(byte => byte !== 0)) throw new Error("The plaintext was retained.");
  await saveBackup(key, backup);
  const loaded = await loadBackup(id);
  if (!loaded) throw new Error("The backup was not saved.");
  const actual = await decryptBackup(loaded.key, loaded.backup);
  if (actual.some((byte, index) => byte !== expected[index])) throw new Error("Recovery changed the saved data.");
  const wrong = await importBackupKey(crypto.getRandomValues(new Uint8Array(32)));
  const changedId = (id[0] === "a" ? "b" : "a") + id.slice(1);
  let refused = 0;
  for (const [testKey, input] of [[wrong, backup], [key, {...backup, id: changedId}], [key, {...backup, iv: backup.iv.slice(0, -2)}]]) {
    try { await decryptBackup(testKey, input); } catch { refused++; }
  }
  actual.fill(0);
  expected.fill(0);
  if (refused !== 3) throw new Error("A changed backup or wrong key was accepted.");
  return id;
}

export async function checkAfterReload(id) {
  const loaded = await loadBackup(id);
  if (!loaded) throw new Error("The backup was lost after reload.");
  const actual = await decryptBackup(loaded.key, loaded.backup);
  actual.fill(0);
  return !loaded.key.extractable;
}
