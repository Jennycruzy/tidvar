Tidvar: a VPN you pay for by the second with private Zcash, off-chain.

**Each payment is smaller than Zcash's own network fee, and the service never holds your money.**

Open a tab with one shielded payment into a lock that needs both your key and the service's. Your browser
then pays 1 zatoshi per 6 seconds of VPN, signed every 10 seconds, off-chain and instantly. Stop paying and
the VPN cuts off within about 27 seconds. Close the tab and whatever you didn't use comes back to you:
hundreds of payments, two ordinary private transactions on chain. If the service disappears, a time-sealed
refund returns your money without it.

**TESTNET ONLY.** This build accepts testnet TAZ. Do not send mainnet ZEC.

## How the lock works

The browser and the service create a joint signing key. Each keeps its own share; neither can spend the tab alone.
Before showing a funding address, the browser checks the service's time-sealed share and verifies both signatures on the tab's terms.

A payment is a partially signed shielded transaction authorizing a cumulative amount. The service verifies the funding notes, outputs, fee, anchor and browser signature. It saves the newest payment and retains its own unsigned nonce. Replacing a payment erases the replaced nonce.

A cooperative close uses a fresh transaction and signatures. It pays the service for usage and returns the remainder after the network fee. The native prover checks the proof before extracting and submitting the transaction.

## Components

| Directory | Responsibility |
| --- | --- |
| `crates/tidvar-core` | Joint keys, signing, transaction checks, amount rules, usage clock and canonical message hashes |
| `crates/tidvar-seal` | Time-sealed service share and its verification |
| `crates/tidvar-chain` | Testnet transport, compact-note decryption, witnesses and transaction submission |
| `crates/tidvar-payer` | Browser signing state, encrypted-backup contents and recoverable requests |
| `crates/tidvar-payee` | Service-only share storage, payment verification, ledger and native proofs |
| `services/api` | Offers, opening, authenticated payments, VPN key registration, pause and resume, and same-origin wallet-server relay |
| `services/vpn` | Kernel WireGuard controller: admits a key while paid time remains and removes it 5 seconds after paid time ends |
| `packages/tidvar-web` | Browser storage, opening and payment transport helpers |
| `web` | Browser interface and signing worker |
| `deploy` | Service units and network configuration |

## Build

Build on the Linux host with the toolchain pinned in `rust-toolchain.toml`, Node.js 22 and the WebAssembly target:

```sh
rustup target add wasm32-unknown-unknown
cargo build --workspace --release --locked
npm ci --prefix packages/tidvar-web --ignore-scripts
npm ci --prefix web --ignore-scripts
npm run build:wasm --prefix packages/tidvar-web
npm run build --prefix packages/tidvar-web
mkdir -p web/public/kit web/public/pkg
cp packages/tidvar-web/dist/*.js web/public/kit/
cp packages/tidvar-web/pkg/tidvar_payer.js packages/tidvar-web/pkg/tidvar_payer_bg.wasm web/public/pkg/
npm run build --prefix web
```

Install the `wasm-bindgen` CLI version matching the pinned Rust dependency before running `build:wasm`. Generated bundles, browser profiles and private keys do not belong in Git.

See [operations](docs/operations.md) for required configuration and [browser transport](docs/kit.md) for the available interfaces.

## Check

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p tidvar-core -p tidvar-seal -p tidvar-chain -p tidvar-payer -p tidvar-payee --target wasm32-unknown-unknown --locked
npm run check --prefix packages/tidvar-web
npm run check --prefix web
```

Tests requiring real funded notes or a public testnet server are explicitly ignored by the ordinary offline suite. Their descriptions and environment requirements are in the integration-test source. Transaction-submission tests require deliberate operator setup and must not be run as an indiscriminate batch.

## Storage and retries

The browser encrypts its phrase and signing state before writing IndexedDB or exporting a backup. The service database stores the service's share, public terms and received payment material; it does not receive the browser's share.

A complete authenticated payment request is saved before delivery. After a connection loss, the browser retries that request. The service returns its stored receipt for the same request, including after a restart. Keep the encrypted backup and recovery phrase.

## Network fees

The VPN rate is 1 zat per 6 seconds. Payments use whole zat. While visible, the browser authorizes 20 seconds ahead; a hidden page authorizes an hour ahead.

The minimum funding amount is 50,000 zat. A tab reserves 20,000 zat for closing fees and supports up to four funding notes. Closing pays the actual fee for its transaction shape; unused reserve is included in the return.

## Security

Read [SECURITY.md](SECURITY.md) before operating a service or using the signing interfaces. All network access and randomness stay at the edges; the protocol crate performs no network or filesystem operations.
