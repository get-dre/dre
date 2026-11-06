package main

import (
	"context"
	"database/sql"
	"encoding/hex"
	"fmt"
	"strings"
	"unicode"
)

// relationParts accepts only qualified identifiers, including escaped quoted names.
func relationParts(name string) ([]string, error) {
	text := strings.TrimSpace(name)
	var parts []string
	for len(text) > 0 {
		var part string
		if text[0] == '`' || text[0] == '"' {
			quote := text[0]
			text = text[1:]
			closed := false
			for len(text) > 0 {
				c := text[0]
				text = text[1:]
				if c == quote {
					if len(text) > 0 && text[0] == quote {
						part += string(c)
						text = text[1:]
						continue
					}
					closed = true
					break
				}
				part += string(c)
			}
			if !closed {
				return nil, fmt.Errorf("unterminated relation identifier `%s`", name)
			}
		} else {
			end := 0
			for _, c := range text {
				if !(unicode.IsLetter(c) || c == '_' || (end > 0 && unicode.IsDigit(c))) {
					break
				}
				end += len(string(c))
			}
			part = text[:end]
			text = text[end:]
		}
		if part == "" {
			return nil, fmt.Errorf("`%s` must contain only a relation name", name)
		}
		parts = append(parts, part)
		text = strings.TrimSpace(text)
		if text == "" {
			return parts, nil
		}
		if text[0] != '.' {
			return nil, fmt.Errorf("`%s` must contain only a relation name", name)
		}
		text = strings.TrimSpace(text[1:])
		if text == "" {
			return nil, fmt.Errorf("incomplete relation name `%s`", name)
		}
	}
	return nil, fmt.Errorf("empty relation name")
}

func catalogLiteral(value string) string {
	return "decode(unhex('" + hex.EncodeToString([]byte(value)) + "'), 'UTF-8')"
}

type wantedTable struct{ requested, catalog, schema, name string }

// Catalog owns Databricks' metadata SQL; core only validates the returned document.
func (d *databricks) Catalog(tables, schemas []string) (any, error) {
	ctx, done := d.Start(context.Background())
	defer done()
	return buildCatalog(tables, schemas, func(query string) ([][]any, error) {
		rows, err := d.conn.QueryContext(ctx, query)
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		columns, err := rows.Columns()
		if err != nil {
			return nil, err
		}
		var out [][]any
		for rows.Next() {
			values := make([]sql.NullString, len(columns))
			ptrs := make([]any, len(columns))
			for i := range values {
				ptrs[i] = &values[i]
			}
			if err := rows.Scan(ptrs...); err != nil {
				return nil, err
			}
			row := make([]any, len(columns))
			for i, value := range values {
				if value.Valid {
					row[i] = value.String
				}
			}
			out = append(out, row)
			if len(out) >= 100000 {
				return nil, fmt.Errorf("catalogue exceeds 100000 rows; narrow the requested schema")
			}
		}
		return out, rows.Err()
	})
}

func buildCatalog(tables, schemas []string, query func(string) ([][]any, error)) (any, error) {
	if len(tables) == 0 && len(schemas) == 0 {
		return nil, fmt.Errorf("catalog request needs at least one table or schema")
	}
	checked := func(sql string, width int) ([][]any, error) {
		rows, err := query(sql)
		if err != nil {
			return nil, err
		}
		if len(rows) >= 100000 {
			return nil, fmt.Errorf("catalogue exceeds 100000 rows; narrow the requested schema")
		}
		for _, r := range rows {
			if len(r) < width {
				return nil, fmt.Errorf("source returned incomplete catalogue rows")
			}
		}
		return rows, nil
	}
	defaults, err := checked("select current_catalog(), current_schema()", 2)
	if err != nil {
		return nil, err
	}
	if len(defaults) == 0 {
		return nil, fmt.Errorf("connection returned no current catalog/schema")
	}
	catalog, ok := defaults[0][0].(string)
	if !ok {
		return nil, fmt.Errorf("catalog missing")
	}
	schema, ok := defaults[0][1].(string)
	if !ok {
		return nil, fmt.Errorf("schema missing")
	}
	var wanted []wantedTable
	var filters []string
	cmp := func(column, value string) string { return "lower(" + column + ")=lower(" + catalogLiteral(value) + ")" }
	for _, name := range tables {
		parts, err := relationParts(name)
		if err != nil {
			return nil, err
		}
		c, s, n := catalog, schema, parts[len(parts)-1]
		switch len(parts) {
		case 1:
		case 2:
			s = parts[0]
		case 3:
			c, s = parts[0], parts[1]
		default:
			return nil, fmt.Errorf("`%s` needs one to three name parts", name)
		}
		wanted = append(wanted, wantedTable{name, c, s, n})
		filters = append(filters, "("+cmp("table_catalog", c)+" and "+cmp("table_schema", s)+" and "+cmp("table_name", n)+")")
	}
	for _, name := range schemas {
		parts, err := relationParts(name)
		if err != nil {
			return nil, err
		}
		c, s := catalog, parts[len(parts)-1]
		switch len(parts) {
		case 1:
		case 2:
			c = parts[0]
		default:
			return nil, fmt.Errorf("schema `%s` needs one or two name parts", name)
		}
		filters = append(filters, "("+cmp("table_catalog", c)+" and "+cmp("table_schema", s)+")")
	}
	rows, err := checked("select table_catalog, table_schema, table_name, comment from system.information_schema.tables where "+strings.Join(filters, " or ")+" order by table_catalog, table_schema, table_name", 4)
	if err != nil {
		return nil, err
	}
	out := []any{}
	found := map[string]bool{}
	for _, row := range rows {
		c, cok := row[0].(string)
		s, sok := row[1].(string)
		n, nok := row[2].(string)
		if !cok || !sok || !nok {
			return nil, fmt.Errorf("table catalog/schema/name missing")
		}
		columns, err := checked("select column_name, full_data_type, comment from system.information_schema.columns where table_catalog="+catalogLiteral(c)+" and table_schema="+catalogLiteral(s)+" and table_name="+catalogLiteral(n)+" order by ordinal_position", 3)
		if err != nil {
			return nil, err
		}
		cols := []any{}
		for _, col := range columns {
			cols = append(cols, map[string]any{"name": col[0], "type": col[1], "description": col[2]})
		}
		requested := []string{}
		for _, w := range wanted {
			if strings.EqualFold(c, w.catalog) && strings.EqualFold(s, w.schema) && strings.EqualFold(n, w.name) {
				requested = append(requested, w.requested)
				found[w.requested] = true
			}
		}
		out = append(out, map[string]any{"catalog": c, "schema": s, "name": n, "qualified_name": []string{c, s, n}, "description": row[3], "description_source": "database", "columns": cols, "requested": requested})
	}
	for _, w := range wanted {
		if !found[w.requested] {
			return nil, fmt.Errorf("table `%s` is missing or not visible", w.requested)
		}
	}
	policy := sourceDialect()
	return map[string]any{"schema_version": "dre/catalog/v1", "identifier_quote": policy.IdentifierQuote, "dialect": map[string]any{"identifier_quote": policy.IdentifierQuote, "identifier_case": policy.IdentifierCase, "literal_style": policy.LiteralStyle, "hex_literal": policy.HexLiteral}, "current_catalog": catalog, "current_schema": schema, "tables": out}, nil
}
