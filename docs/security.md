# Threat model

Properties S1–S9 are defined in [protocol.md](protocol.md#8-safety-properties).

## What each party learns

| Who | Learns | Never learns |
|---|---|---|
| Anyone watching the chain | Two shielded transactions per tab (the payment into the tab and its close), shaped like most Ironwood transactions ([measurement](../evidence/mainnet/privacy/README.md)) | Amounts, parties, that a tab existed, how many payments were made |
| The service | The tab's value, each payment's amount and time, the connecting IP address (as with any VPN or web service), this tab's return address | Who the person is, their wallet balance, their other addresses or other tabs (each tab has new keys) |
| The public Zcash server | That our relay syncs whole block ranges | The person's IP address; which notes are theirs (decryption happens on the person's device) |
| Our relay (part of the service) | The browser's IP address and which block ranges it asks for | Which notes are the person's |
| drand | Nothing (it publishes every round to everyone) | — |
| The watchtower | Finished close transactions | Keys, shares, nonces, the service database (the process is denied access to both folders) |

## Adversaries

**A malicious service.** It can stop serving at any time, so the person's prepaid time is lost: at most 20
seconds of VPN while the page is open, 60 minutes (600 zat) while it is hidden, or one 1,000-zat chat answer.
It cannot take more than the newest amount the payer signed (S1). It cannot keep the money locked: after the
refund time, the payer refunds alone (S3). It cannot fake the sealed share, because the payer checks 32 of 64
openings before paying, which gives a cheating service a `2^-60.7` chance (§3 of the protocol). It cannot charge
a chat answer or paid request twice, because charging and the signed reply are one database transaction.

**A malicious payer.** It can stop paying; the VPN cuts within 30 seconds (12.8–27.8 s measured on Android,
desktop Chrome and Firefox), and the service closes with the newest IOU before the refund time (S4), alone if
needed, or through the watchtower if the service is down. It cannot spend the tab alone before the refund
time (S2). It cannot present a request or IOU from another tab (S6), reuse a nonce (S5), or underpay inside an
IOU. Every IOU is checked for amounts, outputs, fee, branch, expiry and a real anchor, and is proven whenever the
witness changes.

**A network observer.** It sees HTTPS to the service and WireGuard traffic to the VPN, as with any VPN. On
chain, see the table above.

**A malicious Zcash server.** It can withhold or delay data, which delays the tab. A false view in the browser
cannot move money: the service checks every payment's anchor against its own tree state and proves it with a
real witness, and only real notes can be spent on chain.

**A minority of drand operators.** They cannot publish a round early; that needs the threshold. Every round's
BLS signature is verified before use (`at12_forged_round_signature_fails_bls_verification`). If drand stops,
the refund waits for the round, and the service can still close as normal.

**A thief of the watchtower queue.** They can publish the service's own finished close early. That transaction
pays the service what the payer signed and returns the rest to the payer, so it harms neither.

**A thief of the VPS.** They get the service's key shares and service key, but never a payer's share, so they
cannot spend any tab alone. Open tabs can still be closed with the stolen newest IOUs (paying the service's
address) or refunded by their payers after the refund time.

## Keys and storage

- Payer: a 24-word root phrase; each tab's state is encrypted with a key derived from it (AES-GCM, key not
  extractable) and kept in the browser's IndexedDB, with an encrypted backup download.
- Service: its signing key, per-tab key shares and nonces in `payee.sqlite` (mode 600). Nonces are erased once
  used or replaced.
- Agent: the agent's own tab key and encrypted backup in `~/.tidvar-agent` (mode 600). The tab's size is the
  most the agent can ever spend.

## Logs

The public web server keeps no access log. WireGuard keeps no traffic log. Service logs carry no IP addresses
and no tab ids. Chat prompts and answers are held only for the request and never written.
