#!/usr/bin/env python3
"""Testnet only: pays 0.0005 TAZ from the operator's testnet wallet into a new Tidvar tab, so a judge can try
the whole flow without a wallet. One payment per tab address, a few per visitor per day, a daily cap."""
import hashlib, json, os, re, subprocess, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PRIV = "/root/tidvar-private"
DEVTOOL = PRIV + "/probes/zcash-devtool/target/release/zcash-devtool"
WALLET, AGE, LOCK = PRIV + "/keys/testnet-wallet", PRIV + "/keys/testnet-age.txt", PRIV + "/wallet.lock"
STATE = "/root/tidvar-data/faucet.json"
AMOUNT, DAILY, PER_VISITOR = 50000, 60, 4
ADDRESS = re.compile(r"^utest1[02-9ac-hj-np-z]{100,400}$")
lock = threading.Lock()

def load():
    try:
        return json.load(open(STATE))
    except (OSError, ValueError):
        return {"sent": {}, "days": {}}

def save(state):
    tmp = STATE + ".new"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)

def send(address):
    cmd = (f"{DEVTOOL} wallet -w {WALLET} sync --server testnet.zec.rocks:443 --connection direct >/dev/null 2>&1 && "
           f"{DEVTOOL} wallet -w {WALLET} send -i {AGE} --address {address} --value {AMOUNT} "
           f"--server testnet.zec.rocks:443 --connection direct")
    result = subprocess.run(["flock", LOCK, "sh", "-c", cmd], capture_output=True, text=True, timeout=900)
    found = re.search(r"^[0-9a-f]{64}$", result.stdout, re.M)
    with lock:
        state = load()
        state["sent"][address] = {"status": "sent" if found else "failed", "txid": found.group(0) if found else None,
                                  "at": int(time.time())}
        save(state)

class Handler(BaseHTTPRequestHandler):
    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("cache-control", "no-store")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass  # no access log: addresses and visitors are not recorded beyond the limits below

    def do_GET(self):
        address = self.path.rsplit("/", 1)[-1]
        entry = load()["sent"].get(address)
        self.reply(200, entry or {"status": "none"})

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(min(int(self.headers.get("content-length", 0)), 2048)) or b"{}")
        except ValueError:
            return self.reply(400, {"error": "Send the tab address as JSON."})
        address = str(body.get("address", ""))
        if not ADDRESS.match(address):
            return self.reply(400, {"error": "That is not a testnet tab address."})
        visitor = self.headers.get("x-real-ip", "")
        day = time.strftime("%Y-%m-%d", time.gmtime())
        with lock:
            state = load()
            if address in state["sent"]:
                return self.reply(200, state["sent"][address])
            today = state["days"].setdefault(day, {"total": 0, "visitors": {}})
            if today["total"] >= DAILY:
                return self.reply(429, {"error": "Today's free testnet coins are used up. Use a testnet faucet instead."})
            key = hashlib.sha256((day + visitor).encode()).hexdigest()[:16]
            if today["visitors"].get(key, 0) >= PER_VISITOR:
                return self.reply(429, {"error": "You have used today's free tabs. Use a testnet faucet for more."})
            today["total"] += 1
            today["visitors"][key] = today["visitors"].get(key, 0) + 1
            state["sent"][address] = {"status": "sending", "txid": None, "at": int(time.time())}
            save(state)
        threading.Thread(target=send, args=(address,), daemon=True).start()
        self.reply(202, {"status": "sending"})

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 27470), Handler).serve_forever()
