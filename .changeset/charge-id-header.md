---
"synapse": minor
---

A non-streamed managed response names its charges in `x-synapse-charge-id`: the idempotency keys of the wallet debits it settled, comma-separated (a cascade bills each attempt). A product can refund the charge for a call it could not use by these keys, through Aether's charge reversal. Absent when nothing was debited (BYOK, a zero cost, an included-allowance product, or a failed settlement).
