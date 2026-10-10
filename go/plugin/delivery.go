package plugin

// The delivery and network rules every file destination shares, written once (the Rust SDK has
// the same rules in dre_protocol::delivery). A destination implements a few primitives on its
// server (Store); Deliver applies if_exists, atomic and temp_dir, and retries on top. Rules also
// parses connect_timeout and timeout, which the plugin applies to its client.

import (
	"errors"
	"fmt"
	"log/slog"
	"math"
	"path"
	"strconv"
	"strings"
	"time"
)

// IfExists is what to do when a file is already at the delivery path.
type IfExists string

const (
	// IfExistsOverwrite replaces it (the default: a rerun replaces a bad file).
	IfExistsOverwrite IfExists = "overwrite"
	// IfExistsError fails this delivery.
	IfExistsError IfExists = "error"
	// IfExistsNumber keeps both: the new file is saved as <name>_2.<ext>, _3, and so on.
	IfExistsNumber IfExists = "number"
)

// Rules are the rules for one delivery.
type Rules struct {
	IfExists IfExists
	// Atomic writes under a temporary name, then renames.
	Atomic bool
	// TempDir is where the temporary file goes: absolute, or relative to the final file's
	// folder. Empty: next to the final file.
	TempDir string
	// Retries is how many times to try again after a temporary error (0: never).
	Retries int
	// ConnectTimeout and Timeout are for the plugin's client: how long to wait for a
	// connection, and how long a read or write may make no progress.
	ConnectTimeout time.Duration
	Timeout        time.Duration
}

// DefaultRules are the rules a destination gets when nothing is set.
func DefaultRules() Rules {
	return Rules{IfExists: IfExistsOverwrite, Atomic: true, Retries: 3, ConnectTimeout: 30 * time.Second, Timeout: 60 * time.Second}
}

// LegacyRules are how destinations delivered before these rules: replace, straight to the final
// name, no retries.
func LegacyRules() Rules {
	r := DefaultRules()
	r.Atomic, r.Retries = false, 0
	return r
}

// RulesFrom reads the rules from a profile target's conn (connect_timeout, timeout, retries)
// and a destination entry's opts (if_exists, atomic, temp_dir), starting from base. aliases
// maps deprecated keys to these ({"upload_timeout": "timeout"}): an alias still works, with a
// warning. It returns the rules and the warnings.
func RulesFrom(base Rules, conn, opts map[string]any, aliases map[string]string) (Rules, []string, error) {
	var warnings []string
	get := func(key string) (any, bool) {
		for _, m := range []map[string]any{opts, conn} {
			if v, ok := m[key]; ok && v != nil {
				return v, true
			}
			for old, nw := range aliases {
				if v, ok := m[old]; ok && v != nil && nw == key {
					warnings = append(warnings, fmt.Sprintf("`%s` is deprecated; use `%s`", old, nw))
					return v, true
				}
			}
		}
		return nil, false
	}
	r := base
	if v, ok := get("if_exists"); ok {
		s, _ := v.(string)
		switch IfExists(s) {
		case IfExistsOverwrite, IfExistsError, IfExistsNumber:
			r.IfExists = IfExists(s)
		default:
			return r, warnings, fmt.Errorf("`if_exists` must be one of `overwrite`, `error`, `number`, got %v", v)
		}
	}
	if v, ok := get("atomic"); ok {
		b, isBool := v.(bool)
		if !isBool {
			return r, warnings, fmt.Errorf("`atomic` must be true or false, got %v", v)
		}
		r.Atomic = b
	}
	if v, ok := get("temp_dir"); ok {
		s, isStr := v.(string)
		if !isStr {
			return r, warnings, fmt.Errorf("`temp_dir` must be a path, got %v", v)
		}
		r.TempDir = s
	}
	if v, ok := get("retries"); ok {
		n, isNum := Number(v)
		if !isNum || n < 0 {
			return r, warnings, fmt.Errorf("`retries` must be a whole number, got %v", v)
		}
		r.Retries = n
	}
	if v, ok := get("connect_timeout"); ok {
		d, err := ParseDuration(v)
		if err != nil {
			return r, warnings, fmt.Errorf("`connect_timeout` %w", err)
		}
		r.ConnectTimeout = d
	}
	if v, ok := get("timeout"); ok {
		d, err := ParseDuration(v)
		if err != nil {
			return r, warnings, fmt.Errorf("`timeout` %w", err)
		}
		r.Timeout = d
	}
	return r, warnings, nil
}

// ParseDuration reads `30s`, `5m`, `2h`, `1500ms`, or a bare number of seconds.
func ParseDuration(v any) (time.Duration, error) {
	bad := fmt.Errorf("must be a duration such as `30s`, `5m` or a number of seconds, got %v", v)
	secs := func(f float64) (time.Duration, error) {
		if f < 0 || math.IsInf(f, 0) || math.IsNaN(f) {
			return 0, bad
		}
		return time.Duration(f * float64(time.Second)), nil
	}
	switch x := v.(type) {
	case float64:
		return secs(x)
	case int:
		return secs(float64(x))
	case string:
		s := strings.TrimSpace(x)
		i := strings.IndexFunc(s, func(r rune) bool { return (r < '0' || r > '9') && r != '.' })
		num, unit := s, ""
		if i >= 0 {
			num, unit = s[:i], strings.TrimSpace(s[i:])
		}
		n, err := strconv.ParseFloat(num, 64)
		if err != nil {
			return 0, bad
		}
		switch unit {
		case "", "s":
			return secs(n)
		case "ms":
			return secs(n / 1000)
		case "m":
			return secs(n * 60)
		case "h":
			return secs(n * 3600)
		}
	}
	return 0, bad
}

// TimeoutFields are the timeouts every network plugin shares (connect_timeout, timeout); read
// them with RulesFrom.
func TimeoutFields() []Field { return DeliveryFields()[:2] }

// DeliveryFields are the connection fields every network plugin shares.
func DeliveryFields() []Field {
	return []Field{
		{Name: "connect_timeout", Description: "how long to wait for a connection (`30s`, `2m`, or seconds)", Default: "30s", Manual: true},
		{Name: "timeout", Description: "how long a read or write may make no progress before it fails", Default: "60s", Manual: true},
		{Name: "retries", Description: "how many times to try again after a temporary error (0: never)", Default: 3, Manual: true},
	}
}

// DeliveryOptions are the options a destination entry takes for these rules: if_exists, atomic
// and temp_dir. A store whose files only appear once complete needs only if_exists.
func DeliveryOptions() []OptionField {
	return []OptionField{
		{Name: "if_exists", Type: "string", Description: "when a file is already at the path: `overwrite` it, fail with `error`, or `number` the new one", Default: "overwrite", Choices: []string{"overwrite", "error", "number"}},
		{Name: "atomic", Type: "boolean", Description: "upload under a temporary name, then rename, so no half-written file appears", Default: true},
		{Name: "temp_dir", Type: "string", Description: "where the temporary file goes (on the same server), for receivers that pick up any new file"},
	}
}

// Caps is what a store can do in one step.
type Caps struct {
	// CreateExclusive: Write with exclusive fails with ErrExists in the same step.
	CreateExclusive bool
	// RenameNoReplace: Rename without replace fails with ErrExists in the same step.
	RenameNoReplace bool
	// VisibleWhenComplete: a file only appears once it's complete (object stores), so no
	// temporary name is needed.
	VisibleWhenComplete bool
}

// ErrExists means something is already at the path.
var ErrExists = errors.New("a file is already there")

// TemporaryError is a failure trying again may fix: a dropped connection, a timeout before
// anything was accepted, a 429 or a 5xx. RetryAfter is the server's Retry-After.
type TemporaryError struct {
	Err        error
	RetryAfter time.Duration
}

func (e *TemporaryError) Error() string { return e.Err.Error() }
func (e *TemporaryError) Unwrap() error { return e.Err }

// Store is a destination's primitives. Paths are the destination's own (outbound/report.xlsx).
type Store interface {
	Caps() Caps
	// Write writes local to remote, creating its folders. With exclusive, it fails with ErrExists
	// if something is there (only asked when Caps().CreateExclusive).
	Write(local, remote string, exclusive bool) error
	// Rename renames from to to, creating to's folders. Without replace, it fails with ErrExists
	// if something is there (only asked when Caps().RenameNoReplace).
	Rename(from, to string, replace bool) error
	Exists(remote string) (bool, error)
	// Delete removes remote; a missing file isn't an error.
	Delete(remote string) error
}

// Delivered is where a delivery landed.
type Delivered struct {
	// Path is the final path (with IfExistsNumber, possibly report_2.xlsx).
	Path string
	// Attempts is how many tries it took (1 without a retry).
	Attempts int
}

// Deliver delivers local to remote under rules. A file already at the path with `if_exists:
// error` fails with an *Error of kind delivery and code file-exists.
func Deliver(s Store, local, remote string, r Rules) (Delivered, error) {
	return deliverWith(s, local, remote, r, time.Sleep)
}

func deliverWith(s Store, local, remote string, r Rules, sleep func(time.Duration)) (Delivered, error) {
	for attempt := 1; ; attempt++ {
		p, err := tryOnce(s, local, remote, r)
		if err == nil {
			return Delivered{Path: p, Attempts: attempt}, nil
		}
		var temp *TemporaryError
		if errors.As(err, &temp) && attempt <= r.Retries {
			wait := temp.RetryAfter
			if wait == 0 {
				wait = backoff(attempt)
			}
			slog.Info(fmt.Sprintf("%s: %v; trying again in %ds (attempt %d of %d)", remote, err, int(wait.Seconds()), attempt+1, r.Retries+1))
			sleep(wait)
			continue
		}
		switch {
		case errors.Is(err, ErrExists):
			return Delivered{}, &Error{Kind: "delivery", Code: "file-exists", Message: fmt.Sprintf("%s already exists (`if_exists: error`)", remote)}
		case attempt > 1:
			return Delivered{}, &Error{Kind: "delivery", Message: fmt.Sprintf("%v (after %d tries)", err, attempt)}
		}
		return Delivered{}, err
	}
}

// backoff is about 1s, then 4s, then 16s, give or take a quarter.
func backoff(attempt int) time.Duration {
	base := math.Pow(4, float64(attempt-1))
	jitter := 0.75 + float64(time.Now().Nanosecond()%1000)/2000
	return time.Duration(base * jitter * float64(time.Second))
}

func tryOnce(s Store, local, remote string, r Rules) (string, error) {
	caps := s.Caps()
	limit := 1
	if r.IfExists == IfExistsNumber {
		limit = 1000
	}
	if r.Atomic && !caps.VisibleWhenComplete {
		temp := tempPath(remote, r.TempDir)
		if err := s.Write(local, temp, false); err != nil {
			_ = s.Delete(temp)
			return "", err
		}
		replace := r.IfExists == IfExistsOverwrite
		for i := 1; i <= limit; i++ {
			to := numbered(remote, i)
			var err error
			if !replace && !caps.RenameNoReplace {
				// No single-step check here (FTP): look, then rename.
				var there bool
				if there, err = s.Exists(to); err == nil {
					if there {
						err = ErrExists
					} else {
						err = s.Rename(temp, to, false)
					}
				}
			} else {
				err = s.Rename(temp, to, replace)
			}
			if err == nil {
				return to, nil
			}
			if errors.Is(err, ErrExists) && i < limit {
				continue
			}
			_ = s.Delete(temp)
			return "", err
		}
	}
	exclusive := r.IfExists != IfExistsOverwrite
	for i := 1; i <= limit; i++ {
		to := numbered(remote, i)
		var err error
		if exclusive && !caps.CreateExclusive {
			var there bool
			if there, err = s.Exists(to); err == nil {
				if there {
					err = ErrExists
				} else {
					err = s.Write(local, to, false)
				}
			}
		} else {
			err = s.Write(local, to, exclusive)
		}
		if err == nil {
			return to, nil
		}
		if errors.Is(err, ErrExists) && i < limit {
			continue
		}
		return "", err
	}
	return "", ErrExists
}

// numbered is remote, or for i > 1 the numbered name: out/report_2.xlsx.
func numbered(remote string, i int) string {
	if i == 1 {
		return remote
	}
	dir, file := splitPath(remote)
	stem, ext := file, ""
	if p := strings.LastIndex(file, "."); p > 0 {
		stem, ext = file[:p], file[p:]
	}
	return fmt.Sprintf("%s%s_%d%s", dir, stem, i, ext)
}

// tempPath is the temporary name for remote: .<name>.dre-part, next to it or in tempDir.
func tempPath(remote, tempDir string) string {
	dir, file := splitPath(remote)
	name := "." + file + ".dre-part"
	switch {
	case tempDir == "":
		return dir + name
	case path.IsAbs(tempDir):
		return strings.TrimRight(tempDir, "/") + "/" + name
	}
	return dir + strings.TrimRight(tempDir, "/") + "/" + name
}

func splitPath(remote string) (string, string) {
	if p := strings.LastIndex(remote, "/"); p >= 0 {
		return remote[:p+1], remote[p+1:]
	}
	return "", remote
}
