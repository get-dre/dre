---
title: "Password-protect PDFs and zip report packs"
description: "Send password-protected PDF statements and AES-256 zip packs: pdf password:, zip: on destinations, secret_columns masking, and the password by another route."
section: build-reports
position: 6
---

# Password-protect PDFs and zip report packs

Finance, payroll and member mail often must send files only the recipient can open: a statement
PDF per member that needs their password, or one zip holding a client's whole monthly pack. DRE
does both during the run, so `hold:` and `dre release` send exactly what was reviewed, and the
passwords never show in DRE's own logs and results. *New in DRE 0.7* (PDF passwords need the
`document` plugin 1.1.0).

Three keys do it:

- **`password:` on a `pdf` output** encrypts each PDF with an open password (AES-256).
- **`zip:` on a destination entry** delivers one zip of the output's files, plus every
  `attach:`ed output's, optionally with a password (AES-256 by default).
- **`secret_columns:` on a query entry** masks those columns' values wherever DRE reports.

DRE never makes up or stores passwords. You make them in SQL (a column per member) or read them
with `env_var()`; when a team needs the file → password list, it's an ordinary output sent
somewhere you control.

## A password PDF per member

```yaml
queries:
  - {query: members, secret_columns: [pdf_password]}   # member_id, email, alt_email, first_name, pdf_password
  - holdings
output:
  - name: statement
    format: pdf
    template: statement.html
    for_each: {query: members, key: member_id}
    password: "{{ each.pdf_password }}"
  - name: statement_mail
    format: message
    for_each: {query: members, key: member_id}
    title: "Your statement, {{ each.first_name }}"
    attach: [statement]
    destination: {profile: member_mail, to: "{{ each.email }}", hold: true}
  - name: password_mail                 # leave this out when the member already knows it
    format: message
    for_each: {query: members, key: member_id}
    title: "Your statement password"
    text: "Your password is {{ each.pdf_password }}."
    destination: {profile: member_mail, to: "{{ each.alt_email }}", hold: true}
```

The password can be anything the member knows, built in SQL, for example their date of birth:
`strftime(date_of_birth, '%d%m%Y') as pdf_password`. Keep PDF passwords to ASCII letters,
digits and punctuation: macOS Preview and Safari can't open a PDF whose password has other
characters (Acrobat, Chrome, Edge and Firefox can). See [PDF and HTML
documents](plugin-document.md#password-protected-pdfs).

## A client pack in one zip

```yaml
output:
  - name: workbook
    format: xlsx
    queries: [summary]
  - name: detail
    format: csv                          # detail_summary.csv, detail_fees.csv
    queries: [summary, fees]
    destination:
      profile: client_sftp
      path: "/out/pack-{{ run.date.yyyymmdd }}.zip"
      attach: [workbook]
      zip: {password: "{{ env_var('PACK_PW') }}"}    # zip: true for no password
```

- **What goes in:** the output's files and every `attach:`ed output's files, flat (no folders).
  Two files with one name are an error, never renamed. A message can't go in a zip.
- **Name:** the destination's `path` (it must end in `.zip`), or the email's `attachment_name`;
  with neither, `<output>.zip` (a per-row output: the row file's name, like `101.zip`).
- **Every destination:** local, S3, Azure, GCS, SFTP, FTP, Databricks Volumes and email all
  receive the zip as one file. `attach:` works on any destination when the entry zips.
- **Per row:** with `for_each`, each row gets its own zip, and `each.*` in the password.
- **One zip, built once:** the zip is built during the run into the run folder. Entries zipping
  the same files the same way share it. A held entry records the zip and its SHA-256, and
  `dre release` sends it unchanged, or refuses if it changed.
- **Encryption:** `encryption: aes256` (default) or `zipcrypto`. AES-256 zips open in 7-Zip,
  WinRAR, Keka, The Unarchiver and most archive tools, but not in Windows Explorer or macOS
  Archive Utility. `zipcrypto` opens there, but it's weak (it can be cracked), so `dre validate`
  warns.
- An empty password is an error, never an unencrypted file. Email's `max_attachment_mb` checks
  the zip.

## Keep passwords out of DRE's records

`secret_columns:` on a query entry lists columns whose values are secrets. Their values become
`*****` everywhere DRE reports: the console, `dre.log`, `run_results.json`, `rows.jsonl`,
`--preview`, error messages, and `--output-json` (which is what dre-mcp answers from). They're
never masked in what DRE delivers: the PDFs, zips and messages, and the held files waiting for
`dre release`, are the data. A password read with `env_var()` in `password:` or
`zip.password` is masked the same way.

`dre validate` warns when `password:` or `zip.password` reads `each.<column>` of a column not
in the driver query's `secret_columns`. Like other secret masking, values shorter than 4
characters aren't masked; don't use passwords that short.

`--output-json` saves secret columns masked, so a later `--from-results` replays `*****`
instead of the passwords.

## The file → password list

When the client wants the mapping, make it an output and send it somewhere they control, never
to the same place as the files it unlocks:

```yaml
queries:
  - {query: password_list, secret_columns: [pdf_password]}   # member_id, pdf_password
output:
  - name: password_list
    format: csv
    queries: [password_list]
    destination: {profile: secure_sftp, path: "/keys/{{ run.date.yyyymmdd }}.csv"}
```

## Not included

No xlsx or docx passwords (zip them instead), no PDF print or copy restrictions, no folders
inside a zip and no other archive formats. DRE doesn't generate or store passwords.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Per-row outputs](per-row-outputs.md) · **Next:** [DRE practices](practices.md)
