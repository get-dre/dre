package main

import (
	"fmt"
	"strings"
	"testing"
)

func TestRelationPartsRejectSQL(t *testing.T) {
	for _, name := range []string{"orders where true", "orders; select 1", "orders join private", "read_files('private')", "orders.", "`unterminated"} {
		if _, err := relationParts(name); err == nil {
			t.Errorf("accepted %q", name)
		}
	}
	parts, err := relationParts("`catalog`.`odd``schema`.`a'b\\c`")
	if err != nil || len(parts) != 3 || parts[1] != "odd`schema" || parts[2] != "a'b\\c" {
		t.Fatalf("parts = %v, %v", parts, err)
	}
}

func TestCatalogOwnsSQLAndEscapesNames(t *testing.T) {
	calls := 0
	document, err := buildCatalog([]string{"`odd'\\name`"}, nil, func(sql string) ([][]any, error) {
		calls++
		if strings.Contains(sql, "odd'\\name") {
			t.Fatalf("unescaped name in %q", sql)
		}
		switch calls {
		case 1:
			return [][]any{{"main", "public"}}, nil
		case 2:
			if !strings.Contains(sql, catalogLiteral("odd'\\name")) {
				t.Fatalf("missing encoded name: %s", sql)
			}
			return [][]any{{"main", "public", "odd'\\name", "table comment"}}, nil
		case 3:
			return [][]any{{"id", "BIGINT", "column comment"}}, nil
		default:
			return nil, fmt.Errorf("unexpected query: %s", sql)
		}
	})
	if err != nil {
		t.Fatal(err)
	}
	value := document.(map[string]any)
	if value["schema_version"] != "dre/catalog/v1" || calls != 3 {
		t.Fatalf("unexpected catalog: %v", value)
	}
	table := value["tables"].([]any)[0].(map[string]any)
	if table["description"] != "table comment" || table["requested"].([]string)[0] != "`odd'\\name`" {
		t.Fatalf("unexpected table: %v", table)
	}
}

func TestCatalogRejectsMissingTablesAndIncompleteRows(t *testing.T) {
	calls := 0
	_, err := buildCatalog([]string{"orders"}, nil, func(string) ([][]any, error) {
		calls++
		if calls == 1 {
			return [][]any{{"main", "public"}}, nil
		}
		return nil, nil
	})
	if err == nil || !strings.Contains(err.Error(), "missing or not visible") {
		t.Fatalf("missing table: %v", err)
	}
	_, err = buildCatalog([]string{"orders"}, nil, func(string) ([][]any, error) { return [][]any{{"only one"}}, nil })
	if err == nil || !strings.Contains(err.Error(), "incomplete") {
		t.Fatalf("incomplete row: %v", err)
	}
}
