# Tidvar

**Pay for a VPN by the second, and for an AI answer by the answer, in private Zcash. Each payment is smaller
than Zcash's own network fee, and the service never holds your money.**

You open a tab with one ordinary shielded payment from your wallet. From then on your browser pays the
service off-chain, every 10 seconds, exactly for what you use: 1 zatoshi per 6 seconds of VPN, 1,000 zatoshi
per chat answer. When you are done, one shielded transaction pays the service what you used and sends the
rest back to you. Hundreds of payments, two transactions on chain.

Tidvar is a payment channel on Zcash with no change to Zcash itself. It ships a VPN, an AI chat, a paywall
for any web API (`tidvar-402`) and an agent with a hard spending limit (`tidvar-agent`).

**[Mainnet app](https://mainnet.tidvar.38-49-216-59.sslip.io)** ·
**[Testnet app](https://tidvar.38-49-216-59.sslip.io)** ·
**[Check it yourself](https://tidvar.38-49-216-59.sslip.io/check)** ·
**[Protocol](docs/protocol.md)** · **[Threat model](docs/security.md)** · **[Developer kit](docs/kit.md)** ·
**[Evidence](evidence/)**

## What one tab costs

| | One payment in your wallet | A Tidvar tab |
|---|---|---|
| Network fee | 10,000 zat (0.0001 ZEC) per payment | 10,000 zat to open (your wallet's send) + 10,000 zat to close, for any number of payments |
| Smallest payment | 10,000 zat would be the fee alone | 1 zat (6 seconds of VPN) |
| Speed | about 75 s for a block | instant: signed in the browser, checked by the service |
| What the chain sees | one shielded transaction per payment | two shielded transactions per tab, shaped like 86.5% of mainnet Ironwood transactions |

## Recorded so far

Every run below is a real transaction on the public chain, re-checked by `tidvar verify` and by the
[check page](https://tidvar.38-49-216-59.sslip.io/check). TESTNET runs pay in TAZ.

| Ending | Runs | What happened |
|---|---|---|
| Cooperative close | 11 | VPN on an Android phone, desktop Chrome and Firefox; chat; a paid API call; an agent's tab |
| Forced close | 1 | The payer left; at its closing deadline the service closed alone with the newest signed amount |
| Refund | 2 | The service never answered; after the refund time the browser signed the refund alone |
| Watchtower | 1 | The service was stopped; a separate process holding no keys published its close |
| Top-up + renew | 1 + 1 | A tab paid past its first payment's limit after a top-up, then rolled into a new tab in one transaction |

**16 tabs, 33 transactions, all on TESTNET** (`evidence/testnet/runs/`). VPN cut-off after payments stop:
12.8–27.8 s, measured on an Android phone, desktop Chrome and Firefox. Pause cuts it in about 1 second.

## One story

An agent opened a 0.0005 TAZ tab and asked Tidvar Chat 29 questions at 1,000 zat each. The 30th was refused by
the tab itself: "The payment amount exceeds the tab limit." Nothing in the agent's code could raise that
limit, because the money sits in a lock that needs both the agent's key and the service's. The close paid
the service exactly 29,000 zat and returned 11,000 (`evidence/testnet/runs/14-agent-spending-limit`).

## Try it

1. Open the [mainnet app](https://mainnet.tidvar.38-49-216-59.sslip.io/open) (ZEC) or the
   [testnet app](https://tidvar.38-49-216-59.sslip.io/open) (TAZ, free from a faucet).
2. Choose an amount (at least 0.0005) and how long the tab lasts: 15 or 30 minutes, 2 hours, 3, 7 or 30 days. That is also when
   your money comes back to you alone if the service disappears.
3. Paste a fresh receive address from your wallet for the change, write down the 24 words, then pay the QR
   code from any Zcash wallet with Ironwood (Zodl, Zingo).
4. After one block: turn on the VPN (WireGuard config or QR), ask Tidvar Chat, top up, renew, or close.
   The meter shows every payment and "on chain: N transactions".

## Check it yourself in 60 seconds

No keys needed. In the browser: [/check](https://tidvar.38-49-216-59.sslip.io/check) fetches every recorded
transaction from the public Zcash server and recomputes its id from the raw bytes. From a terminal:

```sh
cargo install --git https://github.com/Jennycruzy/tidvar tidvar-cli
tidvar verify --network testnet
```

It re-fetches every txid in `evidence/`, checks heights, and checks the files against `MANIFEST.sha256`.

## How it works

1. **Open.** Your browser and the service make a joint key (FROST, two of two) for a new Ironwood address
   that neither can spend alone. The service also seals its key share to a future drand round, the tab's
   refund time; your browser checks 32 of the 64 sealed pieces before you pay. You fund the address with one
   ordinary shielded payment.
2. **Pay.** Every 10 seconds your browser signs an IOU: a signed note that lets the service take this much.
   Each one is a complete shielded transaction paying the service the running total and you the rest, with
   your half of the signature. The service checks it, keeps only the newest, and serves you. Nothing goes on
   chain.
3. **Close.** Together: one transaction pays the service what you used and returns the rest. If you vanish,
   the service closes alone with your newest IOU before the refund time. If the service vanishes, the drand
   round opens its sealed share at the refund time and your browser refunds you alone.

```
 wallet ──1 payment──▶ tab address (2-of-2)                 service
                          │                                    │
 browser ── IOU every 10 s (signed total, off-chain) ────────▶ │ checks, keeps newest, serves VPN / chat / API
                          │                                    │
                          └── close: 1 transaction ─▶ service gets what you used, you get the rest
                              or refund alone after the time seal opens
```

| Who could cheat | How | Why they can't |
|---|---|---|
| The service | Take more than you used | It can only finish an IOU you signed; you sign only the running total |
| The service | Keep your money locked | After the refund time you refund alone with the opened seal |
| The service | Seal a fake share | Your browser checks 32 of 64 pieces first: a cheat passes with probability `2^-60.7` |
| You | Stop paying and keep the VPN | The VPN cuts within 30 seconds; the service closes with your newest IOU |
| You | Refund early | The seal opens only at the refund time; before that your share alone is not a valid signature |
| Anyone | Close a tab twice | The network's nullifier rule rejects the second spend |

## Guarantees

Each property has the file that enforces it and a test or a recorded run ([protocol §8](docs/protocol.md)).

| | Property | Shown by |
|---|---|---|
| S1 | The service never receives more than the newest amount you signed | `at2_forged_payer_share_cannot_be_aggregated` |
| S2 | You cannot spend the tab alone before the refund time | `at1_another_rounds_signature_cannot_stand_in_for_the_refund_round`, `at12_forged_round_signature_fails_bls_verification` |
| S3 | After the refund time, you can always get your money back alone | runs 10 and 13 |
| S4 | The service can always close with your newest IOU, even with its API down | runs 09 and 11 |
| S5 | No signing nonce is used twice | each nonce is erased once used or replaced (`crates/tidvar-payee/src/store.rs`) |
| S6 | A message for one tab is refused for another | `at13_message_for_one_tab_is_refused_for_another` |
| S7 | A tab cannot be paid out twice | network nullifier rule |

## Privacy

What each party learns is in the [threat model](docs/security.md). On chain, a tab is two shielded transactions.
We measured their shape against 2,000 recent mainnet blocks (`tidvar measure`, `evidence/mainnet/privacy/`):
86.5% of 9,716 Ironwood-only transactions have exactly 2 actions, like a Tidvar open and close; the most common
expiry gap is 40 blocks, the one a cooperative close uses.

| Real | With a boundary | Not claimed |
|---|---|---|
| Payments below the network fee, instant | The browser syncs through our relay, so the public Zcash server never sees your IP; we do, as any VPN does | Anonymity from the service at the IP level (a VPN service always sees your IP) |
| 2-of-2 lock; refund after the time seal | One tab's payments are linked to that tab: open a new tab per session to split them | Protection if drand's threshold colludes to publish early |
| Prompts are answered on our server and never stored | A forced or watchtower close carries a far expiry, so it can stand out; cooperative closes do not | Two-way tabs or routing between services |
| | If you vanish without closing, the service keeps what you prepaid: at most 60 minutes of VPN (600 zat) | Outside user numbers (we report only our own runs) |

## Developer kit

Charge for any route in an axum service, 2,000 zat a request ([kit](docs/kit.md)):

```rust
let paywall = tidvar_402::Paywall::open(&Path::new(&data).join("payee.sqlite"), &offer, 2_000)?;
let app = Router::new()
    .route("/paid/fact", get(|| async { "A Zcash payment channel moves value off chain and settles in one shielded transaction." }))
    .layer(from_fn_with_state(paywall, tidvar_402::charge));
```

In the browser, `await tab.fetchPaid(url, progress)` pays and returns the body. A retried request is never
charged twice: the charge and the signed reply are one database transaction.

An agent with a hard spending limit, also as an MCP server (`tidvar-agent mcp`):

```sh
tidvar-agent open --service https://mainnet.tidvar.38-49-216-59.sslip.io --amount 0.0005 --return <your address>
tidvar-agent ask "What is a payment channel?"
tidvar-agent close
```

## Attacks tried

| Attack | Result | Test or run |
|---|---|---|
| Refund before the refund time with another round's signature | Refused | `at1_another_rounds_signature_cannot_stand_in_for_the_refund_round` |
| Service aggregates a close with a forged payer share | Refused | `at2_forged_payer_share_cannot_be_aggregated` |
| Rogue key in the key setup | Setup aborts | `at7_rogue_dkg_package_aborts_the_setup` |
| Forged drand round signature | BLS check fails | `at12_forged_round_signature_fails_bls_verification` |
| A message from one tab presented for another | Refused | `at13_message_for_one_tab_is_refused_for_another` |
| VPN use after payments stop | Cut in 12.8–27.8 s; Pause in about 1 s | runs 02–07 |
| Refund after the service closed | Refused by the network; the page explains | page check, nullifier rule |
| Malformed messages | 486 million fuzz inputs across 8 decoders, 0 crashes | `crates/tidvar-core/fuzz` |

## Engineering decisions

- **No consensus change.** The lock is a FROST 2-of-2 key over one Ironwood note. Every payment is an
  unproven transaction plus a signature share, so it costs milliseconds; only the close is proven.
- **The refund is a time seal, not a script.** Zcash has no shielded time locks, so the service's share is
  encrypted to a drand round (tlock) with a cut-and-choose check, and the browser can rebuild the full key after
  that round.
- **Two network upgrades at once.** Mainnet runs NU6.3 and testnet already runs NU7; every transaction uses the
  consensus branch of its target height, and IOUs switch automatically when NU7 activates.
- **The browser is the wallet.** Compact blocks are decrypted in the browser through our relay; keys, IOUs and
  backups never leave the device unencrypted.
- **Exactly once.** Each request is saved before it is sent and the service keeps its exact reply, so a retry
  after a dropped connection never pays or charges twice.

## Build and check

```sh
rustup target add wasm32-unknown-unknown
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm ci --prefix packages/tidvar-web --ignore-scripts && npm ci --prefix web --ignore-scripts
npm run build:wasm --prefix packages/tidvar-web && npm run build --prefix packages/tidvar-web
npm run build --prefix web
```

Running a service: [operations](docs/operations.md).

| Directory | What it does |
| --- | --- |
| `crates/tidvar-core` | Joint keys, signing, transaction checks, amounts, deadlines, message hashes |
| `crates/tidvar-seal` | The time-sealed share and its check |
| `crates/tidvar-chain` | Light-client transport, note decryption, witnesses, broadcast |
| `crates/tidvar-payer` | The payer's signing state and backups (native and WebAssembly) |
| `crates/tidvar-payee` | The service's shares, payment checks, ledger and proofs |
| `crates/tidvar-402`, `crates/tidvar-cli` | The paywall layer; `tidvar verify`, `measure`, `send-refund` |
| `services/api`, `services/vpn`, `watch` | The service, the WireGuard controller, the watchtower |
| `packages/tidvar-web`, `packages/tidvar-agent`, `web` | The browser kit, the agent, the app |

## Licence and security

MIT or Apache-2.0 ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)); third-party notices in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Security reports: [SECURITY.md](SECURITY.md).
