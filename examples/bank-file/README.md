# Fixed-width bank file

Two synthetic payment records. This is an illustrative layout, not a real bank's upload
specification. Adjust it to the receiver's documented contract.

```bash
dre validate
dre run payments
```

Expected file: `out/payments.txt`. Each record has 48 ASCII bytes, then CRLF:

| Field | Width | Example |
|---|---|---|
| Account ID | 10 | `0000000042` |
| Account name | 20 | `CLIENT A`, followed by spaces |
| Amount | 10 | `0000012345` means 123.45 |
| Date | 8 | `20260131` |

There is no header. The second amount is 67.89. The CI check compares exact bytes,
including padding, decimal conversion and line endings. No destination contacts a bank.
