# Sealed-share corruption

Real captured quicknet beacon and CSPRNG-created seals. Corrupt opened share refused; a challenge selecting exactly the32valid shares passes opening checks, but release fails cleanly because the other32are invalid.

| Valid | Challenges | Accepted | Observed rate | Exact probability |
|---|---|---|---|---|
|32|10000|0|0|1/C(64,32) =5.456656264964734e-19|

The ten thousand trials exercise the production challenge sampler; deterministic per-share cryptographic checks are reused. This is not ten thousand newly generated seals, nor empirical resolution of such a small probability.

Reproduce: `cargo test -p tidvar-seal --test attacks at5 -- --nocapture`.
