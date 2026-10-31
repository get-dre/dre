# Independent report specification

This is a synthetic principal-balance report, not a regulatory definition of arrears.
The rules and literal expected JSON records are the reference, independently of SQL and
the **unexecuted** SAS source. No SAS output comparison or SAS execution is claimed.

Inputs have unique customer and loan IDs. A branch mapping or repayment may have several
versions; `(ID, version)` is unique, and the greatest version wins before joining or filtering
by date. Branch versions are correction records, not historical effective dates. Repayments
are principal reductions, not instalment schedules. All input dates are date-only, all amounts
are synthetic AUD, and an empty CSV field means null. Customer names may be null.

1. Include a loan when `opened <= report_date` and `closed` is null or `closed > report_date`.
   A loan closed on the report date is excluded. The supplied dates are month ends; SQL also
   accepts other valid dates for investigation. The JSON oracle covers only the two supplied dates.
2. Sum latest-version repayments dated on or before the report date per loan. A repayment
   exactly on the report date counts; a later payment does not. Join that one-row aggregate,
   the unique customer, and one latest branch version to each eligible loan.
3. Emit one exception per eligible loan, using this precedence: missing principal,
   negative principal, missing due date, missing customer, missing branch, then overpayment
   (unrounded principal minus repayments is negative). Exceptions do not enter Detail or Summary.
4. Subtract repayments before rounding. Round each valid loan balance to two decimals, halves
   away from zero. Include only positive rounded balances. Sum those rounded balances; never
   round only after aggregating. Fully paid loans produce neither detail nor exceptions.
5. Days overdue is `max(0, report_date - due_date)` in calendar days. The scheduled due date
   is an input and does not move after a partial payment. Buckets: Current = 0; 1–30 = 1..30;
   31–60 = 31..60; 61–90 = 61..90; Over 90 = 91 onward.
6. Detail contains one record per loan with text loan/customer identifiers, nullable text name,
   branch, product, text ISO due date, numeric integer days, text bucket and numeric balance.
   Summary groups branch/product/bucket, counts accounts, and sums balances. The grouped label
   `branch / product / bucket` is unambiguous for these fixed synthetic labels. Exceptions contains
   text loan ID and reason. All three results sort by their grouping keys or text loan ID.

## Hand-worked controls

At 2026-01-31, loans 1..8 have days 0, 1, 30, 31, 60, 61, 90, 91. Loan 8 uses the long text
ID `123456789012345678901234567890`. Loan 1 rounds 100.005 to 100.01. Loan 2 pays 20 from the
latest `p1` record, not 10+20; its February payment of 5 is excluded. Balance is 80.
The duplicate North branch mapping must not multiply either balance or count.
Loan 3 keeps its null name. Loan 9 opens after cutoff; loan 10 closes exactly on cutoff;
loan 16 pays 50 exactly on cutoff and has zero balance. Those three are absent.
Loans 11..15 produce the five explicit exception reasons in the JSON.

January balances: 100.01 + 80 + 300 + 400 + 500 + 600 + 700 + 800 = **3,480.01**, 8 accounts.
The six branch/product/bucket groups and every detail field are literal in
`expected/2026-01-31.json`.

At 2026-02-28, add 28 days to the original eight overdue values. Loan 2 drops to 75;
loan 9 enters with 13 days overdue and balance 900. The same five exceptions remain.
Total: **4,375.01**, 9 accounts, seven groups. Every changed record and group is explicit in
`expected/2026-02-28.json`. These JSON files are maintained by hand, never regenerated from SQL.

`prepare.py --copies 100` repeats the same independently checkable portfolio 100 times,
suffixing customer, loan and repayment IDs for copies 1..99. Branch mappings remain shared.
That means 1,600 source loans, 400 repayment versions, 800 January detail records, 500 exceptions,
six groups, and balance **348,001.00**. It is a deterministic usability fixture, not a production
banking workload or a database benchmark. Inputs with tied versions or duplicate primary IDs
are outside this fixture's contract and must be resolved before adapting the SQL.
