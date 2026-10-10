package plugin

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestWriteFileAtomic(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "s.json")
	if err := WriteFileAtomic(p, []byte("1"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := WriteFileAtomic(p, []byte("2"), 0o600); err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(p); string(b) != "2" {
		t.Fatal(string(b))
	}
	if es, _ := os.ReadDir(dir); len(es) != 1 {
		t.Fatalf("left behind: %v", es)
	}
	if fi, _ := os.Stat(p); runtime.GOOS != "windows" && fi.Mode().Perm() != 0o600 {
		t.Fatal(fi.Mode())
	}
	// A folder in the way: the rename fails and the temporary file goes.
	sub := filepath.Join(dir, "sub")
	os.Mkdir(sub, 0o755)
	os.WriteFile(filepath.Join(sub, "x"), nil, 0o600)
	if err := WriteFileAtomic(sub, []byte("x"), 0o600); err == nil {
		t.Fatal("wrote over a folder")
	}
	if es, _ := os.ReadDir(dir); len(es) != 2 {
		t.Fatalf("left behind: %v", es)
	}
}
