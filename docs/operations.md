# Running the testnet service

## Configuration

The API uses a real public testnet wallet server and refuses other networks. It listens on loopback; put an HTTPS reverse proxy in front of it for device access.

| Variable | Meaning |
| --- | --- |
| `TIDVAR_SERVICE_ADDRESS` | Required shielded Ironwood testnet receiving address for the service |
| `TIDVAR_SERVICE_KEY` | Private service-identity key file |
| `TIDVAR_DATA_DIR` | Private directory containing `payee.sqlite` |
| `TIDVAR_WEB_ROOT` | Built web directory |
| `TIDVAR_LISTEN` | Loopback bind address |

The shipped API unit reads its environment from `/root/tidvar-private/api-testnet.env`. Create that file with mode 600 and a private parent directory. Do not store its values in Git. The application validates the address and key rather than substituting defaults for missing required values.

The default database directory is `/root/tidvar-data`; its directory permissions are 700 and database permissions are 600. Keep the database, WAL and shared-memory files on the same persistent filesystem.

## Build and start

Build the release executable and web assets before starting the API. Use the executable named in `deploy/tidvar-api.service`. Release builds matter for proof construction; a development executable has materially different timings.

Install the unit, reload systemd and start `tidvar-api.service`. The log line `Tidvar TESTNET API ready.` is emitted after the identity, database, testnet connection and proving key have initialized. Check `GET /v1/offer` on the loopback address before inviting a device to connect.

Do not run two heavy builds or provers together on a constrained host. Keep compiler caches and temporary files outside the application repository.

## Financial state

Retain the service database across deployments. Opening records, consumed signing values, the newest payment, paid time and request counters survive process restarts.

The browser must retain its encrypted IndexedDB state or a downloadable backup and recovery phrase. Losing both sides' recovery material cannot be repaired by creating a new tab with the same address.

Before retrying a submitted transaction, check its actual hash, mining status and expiry. A submitted closing transaction must not be regenerated or funded again because a browser lost its response.

## Transport

Offers and static assets contain no cookies or third-party scripts. The wallet-server relay accepts only named methods and validates framed binary requests.

Authenticated requests contain a tab identifier, increasing counter, previous-message hash and a signature from the payer's tab-record key. The nonce request uses the `Tidvar-Request` header. Payment requests use a JSON body. Bodies are limited to 256 KiB.

Keep access logs free of tab identifiers, wallet addresses, client IP addresses and signing values. The API does not require a request body to be logged for retries to work.

## Browser origin

Encrypted browser state belongs to its origin. Changing scheme, hostname or port creates a different browser storage area. Preserve access to the original origin or import the encrypted backup using the recovery phrase.

A loopback SSH forward is useful for an operator browser, but it is not a device endpoint. Check the forward separately from the host's API health. Use a supervised HTTPS endpoint for a phone session.

## VPN

`tidvar-vpn.service` creates the kernel WireGuard interface `tidvar` (10.77.0.1/16, UDP 51820), loads `deploy/vpn.nft` and adds the ufw rules in `deploy/vpn-network.sh`. It checks every registered key ten times a second: a key is admitted when the tab has at least 20 seconds of unused paid time and is removed 5 seconds after paid time ends, on pause, or on close. Peers are known only by their WireGuard public key; the tab identifier is not written to WireGuard. At most 50 keys are registered.

A second VPN for another network runs from the same binary: `tidvar-vpn-main.service` sets
`TIDVAR_VPN_INTERFACE=tidvarm`, `TIDVAR_VPN_PORT=51821` and `TIDVAR_VPN_NET=78` (10.78.0.1/16) in
`/root/tidvar-private/vpn-mainnet.env`, with its own data directory, key and DNS (`tidvar-dns-main.service`,
`deploy/unbound-main.conf`). The mainnet API (`tidvar-api-main.service`) sets the same `TIDVAR_VPN_NET` so the
tunnel's DNS address matches. Without these variables the VPN is the testnet one above.

After a restart no tab is running and no time is counted until the controller admits it again; a tab with at least 20 seconds of unused paid time is re-admitted, and a tab the payer paused stays paused until an authenticated `vpn_resume` request. The service database waits up to 2 seconds for a write lock shared with the API.

`tidvar-dns.service` runs unbound on 10.77.0.1 with query logging off and DNSSEC validation. Install its configuration with `install -m 644 deploy/unbound.conf /etc/unbound/tidvar.conf`; the distribution AppArmor profile only allows unbound to read under `/etc/unbound`. Disable the distribution `unbound.service` so it does not compete with the system resolver.

The browser registers its WireGuard public key and pauses or resumes with `POST /v1/tabs/{id}/vpn`, using the same authenticated request format as payments, with body `step` set to `vpn_peer` (with `public_key` in base64), `vpn_pause` or `vpn_resume`. The `vpn_peer` reply carries the tunnel address, DNS server, server key and endpoint for the WireGuard configuration.


## Peer bandwidth

The network setup installs an HTB root on the WireGuard interface. Before admitting a peer, the controller creates a 20 Mbit/s egress class for its destination address and a 20 Mbit/s ingress policer for its source address. Classes use the allocated private address, never a tab identifier.

The controller keeps the installed classes for a peer across Pause and Resume. It admits at most one new key per polling cycle so a burst of admissions does not hold up payment-time removals. If a traffic-control command fails, that key is not admitted.


## Persistent data allowances

The controller samples the kernel WireGuard receive and transmit counters every 100 ms. It stores the counter baseline, per-tab UTC-day total and shared UTC-calendar-month total through the existing SQLite connection. Repeated snapshots do not add bytes again. A peer restart resets its baseline, not the accumulated allowances.

The default per-tab allowance is 5,000,000,000 bytes per UTC day. The server allowance is 6,000,000,000,000 bytes per UTC month across its WireGuard peers. When a sampled total reaches its allowance, the controller pauses billing, removes the peer and preserves prepaid credit. Resume is refused while the allowance remains exhausted. New UTC periods have separate buckets; historical totals are retained.

The authenticated balance reply includes the caller's daily usage and an optional DATA_LIMIT or HOST_LIMIT reason. It does not disclose other tabs or their individual counters.

For testnet operation, both units optionally read the same private vpn-limits.env file. TIDVAR_DAILY_BYTES and TIDVAR_MONTHLY_BYTES may lower the respective allowances for a bounded real-traffic check; zero values and values above the normal allowances are rejected. Remove those overrides and restart both services before inviting a phone. Keep the actual measured transfer totals across such checks.

## Return unused funds

The page stops new IOUs before requesting a cooperative close through the authenticated
POST /v1/tabs/{id}/close route. A close_start request freezes paid-time accounting
and removes the peer through the controller. The browser syncs the real funding notes,
checks the agreed outputs, and signs a close_submit request for the frozen amount
actually used. The service verifies the browser shares and the transaction, completes
its own shares, and proves it without holding the database mutex during proof generation.

The complete signed raw transaction is saved in SQLite before broadcast. Retrying or
recovering the page reuses that transaction and reads its actual chain status. A
close_status request reports the transaction ID, actual fee, returned amount and
mined height. If an attempt expires without appearing on chain, a fresh close can be
signed; previous attempts remain in the database. The page hides funding controls
once a close is submitted. Keep the database and browser backup until the returned
funds have been received.

## Browser recovery and timing observations

Download the latest encrypted backup before moving to another browser, and close the
old page first. The recovery form unlocks it locally with the saved words; neither
the words nor the payer share is sent to the service. Reloading a closing tab resumes
the same closing transaction. Backups downloaded after payments contain the current
request counter and saved signing state.

Stop new payments cancels the browser's payment loop without sending Pause. A payment
already in flight finishes first. The connection monitor then records actual successful
and failed HTTPS requests, with a timeout; it does not claim that ordinary internet
reachability proves the VPN is on. Existing paid time, including an away payment,
remains valid until its deadline. Use this check before paying for background time
if measuring a short cutoff. Pause requests an immediate cut and preserves prepaid time.

Download timing observations exports local sync durations, available browser memory
and WASM size measurements, actual payment signing and delivery durations, visibility
changes, timer firings and closing receipts. Unsupported memory measurements are null.
No backup words, payer shares or WireGuard private keys are included.

## HTTPS for a phone

The API listens on loopback. The optional tidvar-https service forwards it through a
Cloudflare Quick Tunnel using a separately verified cloudflared executable. Install
the executable at /root/tidvar-private/bin/cloudflared and an empty configuration
file at /root/tidvar-private/https/empty.yml. Install deploy/tidvar-https.service,
reload systemd and start it. Read the assigned HTTPS hostname from its journal,
then check the page, offer and WASM resources externally before using it on a phone.

Systemd supervises the tunnel independently of a terminal session. Quick Tunnel
hostnames are temporary and can change when the tunnel process restarts. Keep a
current encrypted backup so the recovery form can recover the tab on a new origin.

## Observe testnet funding

The funding_observer example is an operator measurement tool. Set
TIDVAR_OBSERVER_DATABASE to the existing service database and
TIDVAR_OBSERVER_AFTER_ROW to its current maximum tabs.rowid before starting.
Run cargo run -p tidvar-payee --example funding_observer --locked. It reads only new
tab records, verifies their signed terms, connects to the testnet GetMempoolStream,
and trial-decrypts Ironwood outputs to the exact tab receiver. Its JSON output
records first-seen timestamps, transaction IDs, stream reconnects and gaps. Store
that output privately. It does not sign or broadcast, and does not change the
one-confirmation requirement for VPN activation. A first-seen time alone does not
establish broadcast-to-seen latency: compare it with the wallet send time recorded
on the phone. Historical validation output is explicitly labelled as historical.

## Check a saved close without the browser

Set TIDVAR_STATUS_DATABASE to the service database and TIDVAR_TAB_ID to the tab's
public hexadecimal identifier, then run cargo run -p tidvar-payee --example close_status --locked.
This opens one read-only SQLite connection, closes it before the network query,
and checks the saved raw transaction against the actual testnet response. It
reports the mined height and confirmations only after matching the raw bytes and
transaction ID. It does not sign, broadcast, or modify the database. A saved
height of zero can mean the browser stopped polling before confirmation; check
the chain before considering another transaction.

## Measure a short cutoff on one phone

Prepare short cutoff check is an explicit measurement mode. Use it before setting
up WireGuard. It keeps foreground IOUs enabled and suppresses the hour-long away
payment while switching apps. Import the configuration with the tunnel off, return
to the page for a current payment, enable the tunnel, return again, then stop new
payments while the connection is live. The timing log records when the mode was
prepared and each skipped away action. Existing away credit is not erased or
silently shortened; the page refuses to call an hour of prepaid time a short check.

Restart payments ends this mode and restores normal away payments for background
use. Closing and recovering a tab retain their existing signing and transaction
rules. Open another tab appears only after the current close is confirmed mined;
it clears the current selection while preserving the saved encrypted tab records.
The WireGuard QR is collapsed by default so the phone can import a file without
showing its VPN key in the recording.

## Fund a browser tab from the testnet operator wallet

The funding_request example takes TIDVAR_STATUS_DATABASE and TIDVAR_TAB_ID,
verifies the signed testnet record, and prints its joint receiver and agreed
amount. It uses Store's existing connection handling and does not load or export
the browser payer state. It does not sign or broadcast. Before sending from a
separate testnet wallet, check that no funding was already observed and retain
an exclusive attempt record so retries cannot create duplicate deposits.

WireGuard downloads include a short tab identifier in their filename, such as
tidvar-abcdef.conf. This keeps a new tunnel distinct from an earlier closed tab
when importing it on the same phone.
