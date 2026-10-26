# Security

This software is restricted to public testnet. Do not send mainnet ZEC.

## Keys

The payer and service generate independent signing shares. The service never receives the payer's share or recovery phrase. The payer verifies the service's sealed share before funding.

The service has an identity key, its per-tab signing shares and WireGuard keys. Keep their files private, out of Git, bundles and logs. The browser protects its recovery material with authenticated encryption and checks the tab identity when restoring it.

The time-sealed share relies on drand's threshold operators not publishing a future round early. If the time signal stops, opening that share waits for publication.

## Payments

A received payment is checked for its funding notes, recipients, amounts, fee, branch, expiry, real anchor, witnesses and payer signature. The service retains its own nonce until settlement; replacing an IOU erases the replaced nonce.

Cumulative amounts must increase and remain inside the signed funding limit. A cooperative close cannot exceed the payer's authorized amount. Proof extraction is checked before submission.

Request counters, previous-message hashes and payer signatures prevent a request from being moved into another exchange. The service keeps exact retry replies. A browser must persist its complete request before sending it.

## Recovery material

Keep the phrase and encrypted backup. A server cannot reconstruct the browser's signing share from its database.

Encrypted backup files can contain sensitive viewing and signing state after decryption. Never paste decrypted backups into an issue, support message or chat. Report an error using the visible message and transaction hash where available.

## Reporting a problem

Do not post keys, phrases, private signing values or wallet files. Send the maintainer a minimal description and public reproduction steps through an existing private contact channel. No paid disclosure program is offered.
