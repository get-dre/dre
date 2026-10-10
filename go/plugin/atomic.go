package plugin

import (
	"fmt"
	"log/slog"
	"math/rand/v2"
	"os"
	"path/filepath"
)

// WriteFileAtomic writes data to path so a reader, or a crash, sees the old file or the new one,
// never a part: a temporary file (.<name>.<pid>.<random>.tmp) in the same folder, flushed to
// disk, renamed over path, then the folder flushed (a folder that can't be, such as some network
// mounts, is logged at debug and ignored). The Rust side is dre_protocol::util::write_atomic.
func WriteFileAtomic(path string, data []byte, perm os.FileMode) error {
	dir := filepath.Dir(path)
	tmp := filepath.Join(dir, fmt.Sprintf(".%s.%d.%04x.tmp", filepath.Base(path), os.Getpid(), rand.IntN(1<<16)))
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm)
	if err != nil {
		return err
	}
	_, err = f.Write(data)
	if err == nil {
		err = f.Sync()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = os.Rename(tmp, path)
	}
	if err != nil {
		os.Remove(tmp)
		return err
	}
	if d, err := os.Open(dir); err == nil {
		if err := d.Sync(); err != nil {
			slog.Debug(fmt.Sprintf("can't flush the folder %s: %v", dir, err))
		}
		d.Close()
	}
	return nil
}
