# Topic: password-protected PDFs and zip packs

Load this when files must open only for their recipient: a password PDF statement per member,
one password zip holding a client's pack, or keeping passwords out of DRE's logs. Needs DRE 0.7.0 or later; PDF passwords need the `document` plugin 1.1.0 or later. DRE never
makes up or stores passwords: the user builds them in SQL (a column per member, often from
something the member knows) or reads one with `env_var()`. Never write a password into a file.

## A password statement per member

```yaml
queries:
  - {query: members, secret_columns: [pdf_password]}   # member_id, email, alt_email, first_name, pdf_password
  - holdings
output:
  - name: statement
    format: pdf
    template: {file: statement.html}
    for_each: {query: members, key: member_id}
    password: "{{ each.pdf_password }}"
  - name: statement_mail
    format: message
    for_each: {query: members, key: member_id}
    title: "Your statement, {{ each.first_name }}"
    attach: [statement]
    destination: {profile: member_mail, to: "{{ each.email }}", hold: true}
  - name: password_mail                 # leave out when the member already knows it (a birth date)
    format: message
    for_each: {query: members, key: member_id}
    title: "Your statement password"
    text: "Your password is {{ each.pdf_password }}."
    destination: {profile: member_mail, to: "{{ each.alt_email }}", hold: true}
```

- The password goes by another route than the file, or not at all (RUN-10). Ask which.
- Every column a password reads is in its query's `secret_columns:` (RUN-11): then it shows as
  `*****` in the console, `dre.log`, `run_results.json`, `rows.jsonl`, previews and
  `--output-json`. `dre validate` warns otherwise. Delivered files and messages keep the values.
- PDF passwords: ASCII letters, digits and punctuation, 8+ characters (macOS Preview can't open
  others). A date of birth in SQL: `strftime(date_of_birth, '%d%m%Y')`.

## A client pack in one zip

```yaml
output:
  - name: workbook
    format: xlsx
    queries: [summary]
  - name: detail
    format: csv
    queries: [summary, fees]
    destination:
      profile: client_sftp
      path: "/out/pack-{{ run.date.yyyymmdd }}.zip"   # must end in .zip
      attach: [workbook]
      zip: {password: "{{ env_var('PACK_PW') }}"}       # zip: true for none
```

- `zip:` works on every destination; `attach:` adds other outputs' files (never a message). The
  zip is flat; two files with one name are an error.
- AES-256 is the default (RUN-12): recipients need 7-Zip, WinRAR, Keka or similar.
  `encryption: zipcrypto` opens in Windows Explorer but is weak; validate warns.
- With `for_each`, each row gets its own zip and `each.*` in the password.
- A file → password list is an ordinary output (`secret_columns` on its query) to a destination
  the client controls, never the one receiving the files.

Docs: [Password-protect PDFs and zip report packs](../docs/password-protected-outputs.md).
