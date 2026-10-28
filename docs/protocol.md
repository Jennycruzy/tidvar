# Tidvar protocol

A payment channel on Zcash with no consensus change: two-party signing over one Ironwood note, and a
refund key sealed to a public drand round. This document describes the code in this repository; every rule
below names the file that enforces it.

## 1. Parties and parameters

- **P** pays (a browser, the `tidvar` command, or an agent). **S** is the service.
- Amounts are in zatoshi (1 ZEC = 100,000,000 zat).
- `H(tag, …)` is BLAKE2b-256 with a 16-byte personalization starting with `Tidvar`, over a length-prefixed
  encoding (`crates/tidvar-core/src/encoding.rs`).

Fixed when the tab opens, inside the tab record P and S both sign (`crates/tidvar-core/src/record.rs`):

| Name | Value | Where |
|---|---|---|
| `V` funded | at least 50,000 zat, paid by one ordinary shielded payment (a wallet may split it into up to 4 notes) | `params.rs` `spendable` |
| `F` fee reserve | 20,000 zat; `M = V − F` is the most S can ever be paid | `params.rs` |
| `C(n)` close fee | `5,000 · max(2, n)` for `n` tab notes (ZIP-317, matching ordinary wallets); `F − C(n)` returns to P | `params.rs` `close_fee` |
| Lifetime `L` | 2 hours, 3, 7 or 30 days; 15 minutes on testnet only (to show the refund end to end) | `params.rs` `Lifetime`, `record.rs` `Terms::validate` |
| Deadlines | `T_refund = T_open + L`; `T_pack = T_refund − min(18 h, 7L/8)`; `T_watch = T_refund − min(12 h, 3L/4)`; `T_close = T_refund − min(6 h, L/2)`; always `T_open < T_pack < T_watch < T_close < T_refund` | `params.rs` `Deadlines` |
| Refund round `R` | the drand quicknet round at `T_refund` | `tidvar-seal` `round_at` |
| IOU expiry | `h_open + ceil(L / 20 s)` blocks, so a payment transaction outlives `T_close` on every network | `params.rs` `iou_expiry` |
| Network | mainnet or testnet, in the offer; transactions use the consensus branch of their target height (Ironwood: NU6.3 on mainnet, NU7 on testnet) and the network's coin type | `record.rs` `Network` |

## 2. Keys: a 2-of-2 lock without a dealer

1. P and S run FROST distributed key generation (`frost-core` 3.0 through `reddsa` `frost::redpallas`), P = 1,
   S = 2, two signers of two. Round-one packages carry proofs of knowledge, so a rogue key aborts the setup
   (`keys.rs`; test `at7_rogue_dkg_package_aborts_the_setup`).
2. The group key is made even-y so it is a valid Ironwood `ak`.
3. Each party commits to 32 random bytes in round one and reveals them in round two; the full viewing key's
   `nk` and `rivk` derive from both, so both can see the tab and neither can spend alone.
4. The tab address is that viewing key's external address with an Ironwood receiver only. Every tab has new
   keys.
5. The tab id is `H("TidvarTabId", group key ‖ viewing key ‖ offer ‖ terms ‖ seal commitment)`.

## 3. The sealed refund key

Before P pays, S proves that drand round `R` will release its share `s2` (`crates/tidvar-seal/src/seal.rs`):

1. S splits `s2` with a degree-32 polynomial into 64 shares and publishes Feldman commitments to it.
2. Each share is encrypted to round `R` with timelock IBE (with a recorded `sigma`) and masked with
   `ChaCha20(H("TidvarSealKey", k))`.
3. S commits to all of it (`H("TidvarSealCommit", …)`). Only then does P pick 32 indices at random.
4. S opens those 32. P checks each against the commitments and re-encrypts it byte for byte. Any mismatch
   and P refuses to fund. A cheating S passes with probability `1 / C(64, 32) ≈ 2^-60.7`.
5. After `R`, anyone can fetch the drand signature for `R`. P verifies it (BLS), decrypts the 32 unopened
   shares, keeps those that match their commitments and interpolates `s2`, then checks `[s2]B` equals S's
   public verifying share.

Tests: `full_seal_checks_thirty_two_openings_and_recovers_service_share`,
`a_second_challenge_cannot_reveal_another_half`, `changed_opening_and_changed_service_point_are_refused`,
`original_tlock_and_exact_encryption_are_compatible`, `at12_forged_round_signature_fails_bls_verification`,
`at1_another_rounds_signature_cannot_stand_in_for_the_refund_round`.

## 4. Opening a tab

| Step | What happens | Who could cheat | Why they can't |
|---|---|---|---|
| 1 | P fetches `GET /v1/offer`: network, service name, `addr_S`, maximum lifetime, price, S's service key | S lies about prices | The offer is inside the record P signs; the page shows it before funding |
| 2 | DKG rounds one and two | either party: rogue key | proofs of knowledge |
| 3 | S seals `s2` and commits; P challenges; S opens; P verifies | S seals garbage | cut and choose (§3) |
| 4 | Both sign the tab record (P with a key from its own root secret, S with its service key) | either denies the terms | both signatures over the same record |
| 5 | The page shows the address and amount; the person pays from their own wallet | — | an ordinary shielded payment |
| 6 | Both find the note with the viewing key; S waits for one confirmation and checks the value is exactly `V` | P claims to have paid | S serves only after seeing the note itself |

## 5. Paying: IOUs

An IOU is an unproven transaction that spends the tab's notes and has two outputs: `x` to `addr_S` and
`V − x − C(n)` to `addr_P`, plus P's FROST signature share for it (`crates/tidvar-core/src/iou.rs`).

1. P builds it at the anchor it last refreshed (at most 24 blocks old), with the IOU expiry, empty memos and
   fresh randomness, then signs its share over the shielded sighash with the spend's randomizer.
2. P sends it with `POST /v1/tabs/{id}/iou`. S refuses unless: `x` is larger than the last payment and at most
   `M`; the transaction spends exactly the tab's notes, pays exactly `C(n)`, has exactly the two right outputs
   and the right branch and expiry; the anchor is a real Ironwood root; a proof builds (whenever the anchor
   or witness changed); P's share verifies (`crates/tidvar-payee/src/payments.rs`).
3. S keeps only the newest IOU and the one nonce that can complete it; replaced nonces are erased
   (`crates/tidvar-payee/src/store.rs`).

Cadence (`web/src/main.ts`): every 10 seconds while the page is visible, 20 seconds ahead; one payment 60
minutes ahead when the page is hidden (sent on the hide event, or by the page's timer if the browser skipped
the event); back to every 10 seconds on return.

**Products on one tab** (`crates/tidvar-core/src/ledger.rs`): `used = ceil(VPN seconds / 6) + per-request
charges`. The VPN admits a peer while `paid ≥ used + 4` and removes it once paid time ends plus 5 seconds.
A per-request charge (Tidvar Chat at 1,000 zat; any `tidvar-402` route at its price) is a request P signs on
the tab's message channel. S charges it from prepaid credit and records the reply in one database transaction,
so a retried request is never charged twice (`Store::charge_and_reply`). A chat answer the model can't produce
is not charged.

**Top-up** (`Payer::top_up_request`, `services/api/src/payments.rs`): P pays more into the same tab address
from any wallet. The tab counts at most four notes; their sum `V` must be at least the tab's amount, and
anything beyond it raises `M` by exactly that value. Each IOU spends the first `k` counted notes in chain
order, where `k` is the number of signature shares it carries: P uses no more notes than S last reported
counting, and S checks the IOU against exactly those `k` notes, so a top-up that one side has not seen yet
never makes a payment fail. The fee is `C(k)`, and closes spend every note both sides have counted. A top-up
neither side has counted yet stays at the tab address and goes back to P in the refund.

## 6. Ending a tab

| Ending | Who acts | What is published | Where |
|---|---|---|---|
| Cooperative close | P and S together | a fresh transaction paying S `used` and returning the rest to P | `services/api/src/close.rs`, `crates/tidvar-payee/src/close.rs` |
| Renew | P and S together | one cooperative close whose change output is the funding of P's next tab: P opens the next tab (new keys, new seal, new refund time) for exactly what the close returns, then signs the close to that tab's address; S signs only if the next tab is its own and its amount matches | `Tab.renew`, `Payer::prepare_renew_request`, `iou::check_to`, `services/api/src/close.rs` |
| Forced close | S alone, any time, and automatically at `T_close` | S completes the newest IOU with its stored nonce, proves and broadcasts | `services/api/src/force.rs` `force_close`, `Store::sign_forced` |
| Watchtower close | a separate process holding no keys | at `T_pack`, S finishes the newest IOU into a complete transaction and queues it; from `T_watch` the watchtower publishes it if the tab is still open | `force.rs` `pack`, `watch/src/main.rs` |
| Refund | P alone, after `T_refund` | the browser opens the seal with round `R`, rebuilds S's key package from the released share (refused unless it matches S's verifying share), signs every spend with both shares and saves the signed transaction; `tidvar send-refund` proves and broadcasts it | `crates/tidvar-payer/src/engine.rs` `refund_transaction`, `crates/tidvar-core/src/iou.rs` `refund`, `crates/tidvar-cli` |

A packed watchtower transaction is only a floor for S. Payments continue after it, and the newest IOU still
closes the tab. If the tab already closed, the notes are spent and any other close is rejected by the network's
nullifier rule. The page explains that a closed tab has nothing to refund.

## 7. Messages

HTTPS JSON. Every request P sends carries the tab id, a counter `k` and the hash of the previous message, and
is signed with P's tab-record key, so a transcript is a hash chain (`crates/tidvar-core/src/transport.rs`).
S answers each request once and replays the same reply for a retry. No cookies; the page connects only to its
own origin and to public drand relays (for the refund).

## 8. Safety properties

| # | Property | Enforced in | Test or evidence |
|---|---|---|---|
| S1 | S never receives more than the newest amount P signed | P signs only increasing `x ≤ M`; closing needs P's share | `at2_forged_payer_share_cannot_be_aggregated` |
| S2 | P cannot spend alone before `R` | 2-of-2 key; `s2` sealed to `R` | `at1_…`, `at12_…` |
| S3 | P gets `V − C(n)` back alone after `R` if S never closed | seal verified before funding | evidence `testnet/runs/10-refund-alone` |
| S4 | S can always close with the newest IOU before `T_close` | IOU expiry; forced close; watchtower | evidence `09-forced-close`, `11-watchtower-api-stopped` |
| S5 | No nonce is used twice | used nonces are erased; a used commitment is refused | `service_checks_payer_before_consuming_nonce_and_sdk_verifies_final_signature` |
| S6 | Every request and IOU is bound to one tab | signed messages carry the tab id; the sighash covers the tab's notes | `at13_message_for_one_tab_is_refused_for_another` |
| S7 | A tab can close only once | the network's nullifier rule | the refund on a force-closed tab is refused |
| S8 | An older IOU can only cost S, never P | amounts only increase | — |
| S9 | Malformed input is refused cleanly | strict decoders | 8 fuzz targets in `crates/tidvar-core/fuzz`, 30 minutes each |

Check every recorded transaction yourself:

```sh
cargo install --git https://github.com/Jennycruzy/tidvar tidvar-cli
tidvar verify --network testnet
```
