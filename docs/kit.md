# Developer kit

The browser package is an ES module around the WebAssembly payer. Build its WebAssembly bindings and TypeScript before importing it.

## Opening and storage

`configureWasm(url)` selects the generated JavaScript binding. `Opening` owns a `Session` and runs offer verification, two-party key creation, the service-share check, signed terms and local protection before exposing the funding request.

`Opening.recover(id)` loads and verifies an existing encrypted session. `Opening.sync(progress)` retrieves whole compact-block ranges through the same-origin relay, decrypts locally and verifies the reconstructed tree.

`Session.save(id)` queues encrypted writes so an older asynchronous operation cannot overwrite newer signing state. The returned encrypted object is suitable for a backup download. Never upload the plaintext phrase or `backup_plaintext()` result.

## Payments

`new Tab(opening, id)` wraps a recovered or newly accepted opening. Its `pay(hidden, progress)` method refreshes witnesses, obtains unused service commitments, builds and signs the transaction, stores the complete pending request and delivers it.

Call the visible payment method on the service's cadence and call the hidden method when the page becomes hidden. Do not run overlapping payment operations. A signing worker can serialize them while keeping the page responsive.

`Tab.retry()` delivers the exact pending request after recovery. `Tab.nonces()` retrieves the service's signing values and balance. An unsuccessful network request leaves the pending message available for retry; a verified refusal completes its counter exchange.

The payment target uses the payer's own saved usage clock and caps it by the service's reported usage. The service cannot raise the target merely by reporting a larger usage value.

## Top up and renew

`tab.topUp(amountZat)` returns the request for paying more into the same tab address (`address`, `amount`,
`uri` as a ZIP-321 link). Any wallet can pay it. `tab.received()` reports the notes the tab has seen, their
value and how many the service has counted; the tab resyncs as soon as the service reports a newly counted
note, and from then on the extra value can be spent. The tab counts at most four notes.

`tab.renew(openNext, progress)` closes the tab into a new one in a single transaction. `openNext(funded)` must
open the next tab (new keys, new protection, new refund time) for exactly `funded` zat, the amount the close
returns, and give back its `id` and `address`. The service is paid what was used, and the change output funds
the next tab, so the payer's wallet pays nothing. The service signs only if the next tab is its own, on the
same network and for that amount.

## Native service interfaces

`Store` persists accepted records, issued signing values, the newest verified IOU, receipts and usage. `payments::verify` checks a transaction against the accepted record, confirmed funding notes and a real tree root. A changed witness requires a native proof dry run.

`Store::accept_iou` atomically binds each service nonce to its message and updates the newest IOU and paid amount. It does not produce the service's signature. `Store::complete_nonce` produces that share once and saves it before returning; an identical retry returns the stored share.

The caller must use actual chain data for verification and must independently check confirmations. Signing and storage helpers are not permission to trust a browser's reported funding.

## Charge per request: tidvar-402

Any axum service that shares the payee store can put a price on a route
(`services/kit-demo/src/main.rs`, deployed at `/paid/fact`):

```rust
let paywall = tidvar_402::Paywall::open(&Path::new(&data).join("payee.sqlite"), &offer, 2_000)?;
let app = Router::new()
    .route("/paid/fact", get(|| async { "A Zcash payment channel moves value off chain and settles in one shielded transaction." }))
    .layer(from_fn_with_state(paywall, tidvar_402::charge));
```

A request without payment gets `402 Payment Required` with `tidvar-offer` (the service's offer, base64url
JSON) and `tidvar-price`. The payer signs `{"step":"charge","price":…,"route":…}` on its tab's message
channel and sends it in the `tidvar-request` header. The layer charges the tab's prepaid credit and records the
signed reply in one database transaction, then runs the route and returns the reply in `tidvar-reply`. A retry
of the same request is answered from the saved reply and never charged twice. A request that never reached the
route is settled at `POST /v1/tabs/{id}/settle` as not charged.

In the browser: `await tab.fetchPaid(url, progress, {method, body, headers}, maxPrice)` pays the price (plus a
little VPN time while it waits) and returns the body; it refuses a route that asks more than `maxPrice`.

`Paywall::with_routes(store, offer, vec![("/api/search".into(), 3_000), ("/api/*".into(), 500)])` prices
paths instead of a whole router: the first match wins, a trailing `*` is a prefix, and other paths pass free.

**x402 v2.** Every 402 also carries a `PAYMENT-REQUIRED` header (base64 JSON, `x402Version: 2`) whose
`accepts` entry has scheme `tidvar-tab`, network `zcash:mainnet` or `zcash:testnet`, the price in zatoshi
as `amount`, asset `ZEC`, `payTo` (the service's address) and the offer in `extra`. A client may pay with
`PAYMENT-SIGNATURE` (base64 `{x402Version: 2, accepted, payload: {request}}`, where `request` is the signed
charge) and gets `PAYMENT-RESPONSE` (`success`, `network`, `amount`, `payer` = the tab, and the signed reply
in `extensions.tidvar.reply`). `transaction` is empty: the payment is off-chain until the tab closes. The kit
pays over x402 whenever a route offers `tidvar-tab`.

## Proxy: any existing API, no code change

`tidvar-kit-demo` with `TIDVAR_UPSTREAM` puts an existing HTTP API behind tab payments:

```sh
TIDVAR_UPSTREAM=http://127.0.0.1:3000 \
TIDVAR_ROUTES='/api/search=3000,/api/*=500' \
TIDVAR_OFFER_JSON="$(curl -s https://mainnet.tidvar.38-49-216-59.sslip.io/v1/offer)" \
TIDVAR_DATA_DIR=/root/tidvar-data-main TIDVAR_KIT_LISTEN=127.0.0.1:27468 tidvar-kit-demo
```

Priced paths answer 402 until paid; other paths pass through. Payment headers are removed before forwarding,
and `TIDVAR_STRIP=/llm` drops a public path prefix. The price is charged before the request is forwarded, so
an upstream error is still a paid request. Deployed: an unmodified llama.cpp server (the OpenAI chat
completions API) at `/llm/v1/chat/completions`, 1,000 zat a call, on both services.

## Agents: tidvar-agent

`packages/tidvar-agent` runs the same browser package under Node 22. The agent opens its own tab. You fund it
once from your wallet, and the tab's size is the agent's hard spending limit, enforced by the 2-of-2 lock. Its
tab key and encrypted backup live in `~/.tidvar-agent` (mode 600).

```sh
tidvar-agent open --service https://mainnet.tidvar.38-49-216-59.sslip.io --amount 0.0005 --return <your address>
tidvar-agent ask "What is a payment channel?"      # 1,000 zat from the tab
tidvar-agent fetch https://mainnet.tidvar.38-49-216-59.sslip.io/paid/fact
tidvar-agent fetch https://mainnet.tidvar.38-49-216-59.sslip.io/llm/v1/chat/completions \
  --data '{"messages":[{"role":"user","content":"What is a payment channel?"}]}'
tidvar-agent close                                  # the rest returns to your address
```

`TIDVAR_MAX_PRICE` (default 5,000 zat) caps one request; the tab's size caps everything. The person who
starts the agent sets both, so nothing a model reads can raise them.

As an MCP stdio server (`tidvar-agent mcp`), it offers `tab_status`, `ask`, `fetch_paid` (any method and
body) and `close_tab`. Register `node packages/tidvar-agent/agent.mjs mcp` as a stdio server in any MCP client.
Any MCP client:

```json
{"mcpServers": {"tidvar": {"command": "node", "args": ["packages/tidvar-agent/agent.mjs", "mcp"]}}}
```

The testnet service is `https://tidvar.38-49-216-59.sslip.io` (TAZ). Recorded run: an agent tab of 0.0005 TAZ paid for 29 answers. The 30th was refused by the tab itself, and the
close paid the service exactly 29,000 zat (`evidence/testnet/runs/14-agent-spending-limit`).
