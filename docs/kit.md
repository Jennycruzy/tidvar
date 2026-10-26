# Browser transport

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

## Native service interfaces

`Store` persists accepted records, issued signing values, the newest verified IOU, receipts and usage. `payments::verify` checks a transaction against the accepted record, confirmed funding notes and a real tree root. A changed witness requires a native proof dry run.

`Store::accept_iou` atomically binds each service nonce to its message and updates the newest IOU and paid amount. It does not produce the service's signature. `Store::complete_nonce` produces that share once and saves it before returning; an identical retry returns the stored share.

The caller must use actual chain data for verification and must independently check confirmations. Signing and storage helpers are not permission to trust a browser's reported funding.
