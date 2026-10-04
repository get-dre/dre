package main

import (
	"crypto/rand"
	"crypto/rsa"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/array"
	"github.com/apache/arrow-go/v18/arrow/decimal128"
	sf "github.com/snowflakedb/gosnowflake/v2"
	"github.com/youmark/pkcs8"

	"github.com/get-dre/dre/go/plugin"
	"github.com/get-dre/dre/go/plugin/plugintest"
)

// sent runs one batch through what the plugin does before core sees it.
func sent(t *testing.T, rec arrow.Record, cols []column) arrow.Record {
	t.Helper()
	norm, err := normalise(rec, cols)
	if err != nil {
		t.Fatal(err)
	}
	out, err := plugin.Convert(norm, plugin.OutputSchema(declaredSchema(cols)))
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func batch(fields []arrow.Field, build func(b *array.RecordBuilder)) arrow.Record {
	b := array.NewRecordBuilder(mem, arrow.NewSchema(fields, nil))
	defer b.Release()
	build(b)
	return b.NewRecord()
}

func TestIntegerBatchesOfDifferentWidthsGiveOneColumnType(t *testing.T) {
	cols := []column{{name: "N", typ: "FIXED", precision: 18}, {name: "BIG", typ: "FIXED", precision: 38}}
	small := batch([]arrow.Field{{Name: "N", Type: arrow.PrimitiveTypes.Int8}, {Name: "BIG", Type: arrow.PrimitiveTypes.Int8}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int8Builder).AppendValues([]int8{1, -2}, nil)
		b.Field(1).(*array.Int8Builder).AppendValues([]int8{3, 4}, nil)
	})
	wide := batch([]arrow.Field{{Name: "N", Type: arrow.PrimitiveTypes.Int64}, {Name: "BIG", Type: &arrow.Decimal128Type{Precision: 38}}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int64Builder).Append(1 << 40)
		n, _ := decimal128.FromString("123456789012345678901234567890", 38, 0)
		b.Field(1).(*array.Decimal128Builder).Append(n)
	})
	a, b := sent(t, small, cols), sent(t, wide, cols)
	if !a.Schema().Equal(b.Schema()) {
		t.Fatalf("batches differ:\n%s\n%s", a.Schema(), b.Schema())
	}
	if a.Schema().Field(0).Type.ID() != arrow.INT64 || a.Column(0).(*array.Int64).Value(1) != -2 {
		t.Fatalf("%s", a.Schema())
	}
	if got := b.Column(1).ValueStr(0); got != "123456789012345678901234567890" {
		t.Fatal(got)
	}
}

func TestScaledNumbersStayExact(t *testing.T) {
	cols := []column{{name: "AMOUNT", typ: "FIXED", precision: 10, scale: 2}}
	rec := batch([]arrow.Field{{Name: "AMOUNT", Type: arrow.PrimitiveTypes.Int16}}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.Int16Builder).AppendValues([]int16{1234, -5}, []bool{true, true})
	})
	out := sent(t, rec, cols)
	d, ok := out.Column(0).(*array.Decimal128)
	if !ok || d.Value(0).ToString(2) != "12.34" || d.Value(1).ToString(2) != "-0.05" {
		t.Fatalf("%s %v", out.Schema(), out.Column(0))
	}
}

func TestPrettyVariantBecomesCompactJSONAndZonesAreKept(t *testing.T) {
	cols := []column{{name: "V", typ: "VARIANT"}, {name: "TZ", typ: "TIMESTAMP_TZ"}, {name: "NTZ", typ: "TIMESTAMP_NTZ"}, {name: "G", typ: "GEOGRAPHY"}}
	at := arrow.Timestamp(time.Date(2026, 1, 2, 3, 4, 5, 0, time.UTC).UnixMicro())
	rec := batch([]arrow.Field{
		{Name: "V", Type: arrow.BinaryTypes.String},
		{Name: "TZ", Type: &arrow.TimestampType{Unit: arrow.Microsecond}},
		{Name: "NTZ", Type: &arrow.TimestampType{Unit: arrow.Microsecond}},
		{Name: "G", Type: arrow.BinaryTypes.String},
	}, func(b *array.RecordBuilder) {
		b.Field(0).(*array.StringBuilder).Append("{\n  \"b\": \"x,y\",\n  \"a\": [\n    1,\n    2\n  ]\n}")
		b.Field(1).(*array.TimestampBuilder).Append(at)
		b.Field(2).(*array.TimestampBuilder).Append(at)
		b.Field(3).(*array.StringBuilder).Append("{\n  \"coordinates\": [\n    1,\n    2\n  ],\n  \"type\": \"Point\"\n}")
	})
	out := sent(t, rec, cols)
	if v := out.Column(0).ValueStr(0); v != `{"b":"x,y","a":[1,2]}` {
		t.Fatal(v)
	}
	if g := out.Column(3).ValueStr(0); strings.Contains(g, "\n") {
		t.Fatal(g)
	}
	tz, ntz := out.Schema().Field(1).Type.(*arrow.TimestampType), out.Schema().Field(2).Type.(*arrow.TimestampType)
	if tz.TimeZone != "UTC" || ntz.TimeZone != "" {
		t.Fatalf("%s", out.Schema())
	}
	if out.Column(1).(*array.Timestamp).Value(0) != at {
		t.Fatal("instant changed")
	}
}

func TestTempTableSQLCastsTextValues(t *testing.T) {
	s := arrow.NewSchema([]arrow.Field{
		{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
		{Name: "rate", Type: arrow.PrimitiveTypes.Float64, Nullable: true},
	}, nil)
	rec := batch(s.Fields(), func(b *array.RecordBuilder) {
		b.Field(0).(*array.StringBuilder).AppendValues([]string{`it's \x`, ""}, []bool{true, false})
		b.Field(1).(*array.Float64Builder).AppendValues([]float64{1.5, 0}, []bool{true, false})
	})
	sql, n, err := tempTableSQL("dre_lookup_codes", s, []arrow.Record{rec})
	want := "CREATE OR REPLACE TEMPORARY TABLE \"dre_lookup_codes\" AS SELECT $1::VARCHAR AS \"code\", $2::FLOAT AS \"rate\" FROM VALUES\n" +
		"  ('it\\'s \\\\x', '1.5'),\n  (NULL, NULL)"
	if err != nil || n != 2 || sql != want {
		t.Fatalf("%v %d\n%s", err, n, sql)
	}
	empty, _, _ := tempTableSQL("dre_lookup_none", s, nil)
	if empty != `CREATE OR REPLACE TEMPORARY TABLE "dre_lookup_none" ("code" VARCHAR, "rate" FLOAT)` {
		t.Fatal(empty)
	}
}

func keyPEM(t *testing.T, pass string) (string, *rsa.PrivateKey) {
	k, _ := rsa.GenerateKey(rand.Reader, 2048)
	if pass == "" {
		der, _ := x509.MarshalPKCS8PrivateKey(k)
		return string(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})), k
	}
	der, err := pkcs8.MarshalPrivateKey(k, []byte(pass), nil)
	if err != nil {
		t.Fatal(err)
	}
	return string(pem.EncodeToMemory(&pem.Block{Type: "ENCRYPTED PRIVATE KEY", Bytes: der})), k
}

func TestTheAuthenticatorPicksTheSignIn(t *testing.T) {
	base := func(extra map[string]any) map[string]any {
		m := map[string]any{"account": "acme-x", "user": "u"}
		for k, v := range extra {
			m[k] = v
		}
		return m
	}
	plain, key := keyPEM(t, "")
	enc, _ := keyPEM(t, "s3cret")
	path := filepath.Join(t.TempDir(), "rsa_key.p8")
	os.WriteFile(path, []byte(enc), 0o600)
	block, _ := pem.Decode([]byte(plain))
	ok := []struct {
		conn map[string]any
		want sf.AuthType
	}{
		{base(map[string]any{"password": "p"}), sf.AuthTypeSnowflake},
		{base(map[string]any{"authenticator": "username_password_mfa", "password": "p"}), sf.AuthTypeUsernamePasswordMFA},
		{base(map[string]any{"authenticator": "externalbrowser"}), sf.AuthTypeExternalBrowser},
		{base(map[string]any{"authenticator": "oauth", "token": "t"}), sf.AuthTypeOAuth},
		{base(map[string]any{"authenticator": "jwt", "token": "t"}), sf.AuthTypeOAuth},
		{base(map[string]any{"authenticator": "programmatic_access_token", "token": "t"}), sf.AuthTypePat},
		{base(map[string]any{"authenticator": "workload_identity", "workload_identity_provider": "aws"}), sf.AuthTypeWorkloadIdentityFederation},
		{base(map[string]any{"authenticator": "https://acme.okta.com", "password": "p"}), sf.AuthTypeOkta},
		{base(map[string]any{"private_key": plain}), sf.AuthTypeJwt},
		{base(map[string]any{"private_key": base64.StdEncoding.EncodeToString(block.Bytes)}), sf.AuthTypeJwt},
		{base(map[string]any{"private_key_path": path, "private_key_passphrase": "s3cret"}), sf.AuthTypeJwt},
	}
	for _, c := range ok {
		cfg, err := config(c.conn)
		if err != nil || cfg.Authenticator != c.want {
			t.Errorf("%v: %v, %v", c.conn, err, cfg)
		}
	}
	cfg, _ := config(base(map[string]any{"private_key": plain}))
	if !cfg.PrivateKey.Equal(key) {
		t.Error("key not read")
	}
	bad := []struct {
		conn map[string]any
		want string
	}{
		{map[string]any{"user": "u"}, "needs a `account` field"},
		{base(nil), "needs `password`"},
		{base(map[string]any{"authenticator": "saml"}), "unknown `authenticator` `saml`"},
		{base(map[string]any{"authenticator": "oauth"}), "needs `token`"},
		{base(map[string]any{"authenticator": "workload_identity"}), "AWS, AZURE, GCP or OIDC"},
		{base(map[string]any{"private_key_path": path}), "is encrypted; set `private_key_passphrase`"},
		{base(map[string]any{"private_key_path": path, "private_key_passphrase": "wrong"}), "can't read the private key"},
		{base(map[string]any{"private_key": "x", "private_key_path": path}), "not both"},
	}
	for _, c := range bad {
		if _, err := config(c.conn); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%v: %v, want %q", c.conn, err, c.want)
		}
	}
}

func TestDbtFieldsAreAcceptedAndTyposRefused(t *testing.T) {
	if _, err := open(map[string]any{"account": "a", "user": "u", "pasword": "p"}); err == nil || !strings.Contains(err.Error(), "`pasword`") {
		t.Fatalf("%v", err)
	}
	cfg, err := config(map[string]any{"account": "a", "user": "u", "password": "p", "query_tag": "dre", "connect_retries": 2.0, "client_session_keep_alive": true})
	if err != nil || *cfg.Params["query_tag"] != "dre" || cfg.MaxRetryCount != 2 || !cfg.ServerSessionKeepAlive {
		t.Fatalf("%v %+v", err, cfg)
	}
}

func TestTheSourceDescribesItself(t *testing.T) {
	c := plugintest.Start(t, pkg, sourceRole)
	if h := c.Hello(); h["name"] != "snowflake" {
		t.Fatalf("%v", h)
	}
	c.Send(map[string]any{"type": "describe"})
	d := c.Reply()
	if d["identifier_quote"] != `"` || !strings.HasPrefix(plugintest.FieldNames(d), "account,user,authenticator,password,") {
		t.Fatalf("%v", d)
	}
}
