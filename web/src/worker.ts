import {Opening} from "../../packages/tidvar-web/src/opening.js";
import {configureWasm} from "../../packages/tidvar-web/src/wasm.js";
configureWasm("/pkg/tidvar_payer.js");
const opening = new Opening();
const host = globalThis as unknown as {onmessage: ((event: MessageEvent) => void) | null; postMessage(message: unknown): void};
let busy = false;
host.onmessage = async event => {
  if (busy) {host.postMessage({id: event.data.id, error: "This step is still running. Keep the page open."}); return;}
  busy = true;
  try {
    const {id, type, value} = event.data as {id: number; type: string; value: unknown};
    let result: unknown;
    if (type === "protect") result = await opening.protect();
    else if (type === "open") result = await opening.open(value as Parameters<Opening["open"]>[0], text => host.postMessage({progress: text}));
    else if (type === "recover") {
      const id=String(value);
      if (!await opening.recover(id)) throw new Error("This tab was not found. Use your downloaded backup.");
      result={id,backup:await opening.session.save(id),funding:JSON.parse(opening.session.current().funding_request())};
    }
    else if (type === "sync") result = await opening.sync((height, value) => host.postMessage({chain: {height: Number(height), value: Number(value)}}));
    else throw new Error("This step is not available.");
    host.postMessage({id, result});
  } catch (error) {
    host.postMessage({id: event.data.id, error: error instanceof Error ? error.message : "This step could not finish. Keep your backup and retry."});
  } finally {busy = false;}
};
