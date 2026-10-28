# Mainnet transaction shapes

`tidvar measure --network mainnet --blocks 2000 --sample 200` on 28 Oct 2026, blocks 3,530,998–3,532,997,
through https://zec.rocks:443. Raw rows: `ironwood-rows.csv` (every transaction with Ironwood actions) and
`expiry-gaps.csv` (200 evenly spaced Ironwood-only transactions). Summary: `summary.json`.

- 9,716 Ironwood-only transactions; 8,404 of them (86.5%) have exactly 2 actions. A Tidvar open (one payment
  from the person's wallet) and a Tidvar close (one tab note to two outputs) are 2-action transactions.
- Expiry gap (expiry height minus mined height) in the sample: p10 38, p50 125, p90 139 blocks. The single most
  common gap is 40 (30 of 200), the gap a Tidvar cooperative close uses before NU7.
- A forced or watchtower close uses the tab's payment expiry, which lies far beyond these gaps, so that close
  can stand out. Cooperative closes, the normal ending, do not.
