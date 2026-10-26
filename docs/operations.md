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

After a restart every tab starts paused, so time is never counted for a browser that is not present. The browser resumes with an authenticated `vpn_resume` request.

`tidvar-dns.service` runs unbound on 10.77.0.1 with query logging off and DNSSEC validation. Install its configuration with `install -m 644 deploy/unbound.conf /etc/unbound/tidvar.conf`; the distribution AppArmor profile only allows unbound to read under `/etc/unbound`. Disable the distribution `unbound.service` so it does not compete with the system resolver.

The browser registers its WireGuard public key and pauses or resumes with `POST /v1/tabs/{id}/vpn`, using the same authenticated request format as payments, with body `step` set to `vpn_peer` (with `public_key` in base64), `vpn_pause` or `vpn_resume`. The `vpn_peer` reply carries the tunnel address, DNS server, server key and endpoint for the WireGuard configuration.
