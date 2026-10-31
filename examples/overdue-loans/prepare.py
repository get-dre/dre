#!/usr/bin/env python3
"""Render auditable CSV inputs into session-local source tables. No database connection."""

import argparse
import csv
from datetime import date
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SCHEMA = {
    "customers": [("customer_id", "text"), ("customer_name", "text")],
    "branches": [("branch_id", "text"), ("version", "integer"), ("branch_name", "text")],
    "loans": [("loan_id", "text"), ("customer_id", "text"), ("branch_id", "text"),
              ("product", "text"), ("opened", "date"), ("closed", "date"),
              ("due", "date"), ("principal", "amount")],
    "repayments": [("repayment_id", "text"), ("version", "integer"),
                   ("loan_id", "text"), ("paid_on", "date"), ("amount", "amount")],
}


def prepare(database, copies=1):
    if not 1 <= copies <= 1000:
        raise ValueError("copies must be between 1 and 1000")
    oracle = database == "oracle"
    types = {"text": "varchar2(80)" if oracle else "varchar(80)",
             "integer": "number(10,0)" if oracle else "int",
             "date": "date", "amount": "number(18,3)" if oracle else "decimal(18,3)"}
    sql = [] if oracle else ["set nocount on;"]
    for table, columns in SCHEMA.items():
        name = ("ora$ptt_" if oracle else "#") + table
        schema = ", ".join(f"{key} {types[kind]}" for key, kind in columns)
        sql.append(f"create {'private temporary ' if oracle else ''}table {name} ({schema})"
                   + (" on commit preserve definition;" if oracle else ";"))
        with (ROOT / "inputs" / f"{table}.csv").open(newline="") as source:
            rows = list(csv.DictReader(source))
        for copy in range(copies if table in ("loans", "repayments", "customers") else 1):
            for row in rows:
                values = []
                for key, kind in columns:
                    value = row[key]
                    if value and copy and key in ("loan_id", "customer_id", "repayment_id"):
                        value += f"-{copy:04d}"
                    if not value:
                        literal = "null"
                    elif kind in ("amount", "integer"):
                        if kind == "integer":
                            literal = str(int(value))
                        else:
                            amount = Decimal(value)
                            if not amount.is_finite() or amount.as_tuple().exponent < -3:
                                raise ValueError(f"{table}.{key}: expected finite amount with at most 3 decimals")
                            literal = format(amount, "f")
                    else:
                        if len(value) > 80:
                            raise ValueError(f"{table}.{key}: text exceeds 80 characters")
                        literal = "'" + value.replace("'", "''") + "'"
                        if kind == "date":
                            if date.fromisoformat(value).isoformat() != value:
                                raise ValueError(f"{table}.{key}: expected ISO date")
                            literal = f"date {literal}" if oracle else f"cast({literal} as date)"
                    values.append(literal)
                sql.append(f"insert into {name} values ({', '.join(values)});")
    destination = ROOT / database / "reports/overdue/seed.sql"
    destination.write_text("\n".join(sql) + "\n")
    print(f"Prepared {database}: {copies * 16} loans, {copies * 4} repayment versions")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", choices=("oracle", "sqlserver"))
    parser.add_argument("--copies", type=int, default=1)
    args = parser.parse_args()
    prepare(args.database, args.copies)
