# Password statements

Synthetic member statements as password-protected PDFs, the file → password list as a csv, and
a client pack as one AES-256 zip. Everything is delivered to local folders under `out/`. See
[Password-protect PDFs and zip report packs](../../docs/password-protected-outputs.md).

Set the pack's password in your shell (never in a file), then run:

```bash
export PACK_PASSWORD='choose-a-long-one'
dre validate
dre run statements
```

What you get:

| File | Opens with |
|---|---|
| `out/statements/1001.pdf` | `09031984-1001` (Ann's date of birth and member number) |
| `out/statements/1002.pdf` | `23111979-1002` |
| `out/keys/password-list-<date>.csv` | no password: the list the client keeps |
| `out/pack-<date>.zip` (`pack.csv`, `fees.csv`) | `$PACK_PASSWORD`, with 7-Zip, Keka or WinRAR |

The `pdf_password` columns are in `secret_columns`, so no password appears in the console,
`target/run/.../run_results.json`, `rows.jsonl` or `dre.log`: look for `*****` instead. The
PDFs, the list and the zip have the real values.

In production, send the PDFs to members (a per-row `message` with `attach: [statement]`), send
the password by another route or not at all, and deliver the password list to a destination
the client controls, never the one that receives the files.
