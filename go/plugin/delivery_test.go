package plugin

import (
	"errors"
	"fmt"
	"testing"
	"time"
)

// fakeStore is an in-memory store that can fail on cue.
type fakeStore struct {
	caps        Caps
	files       map[string]string
	flaky       int
	breakRename bool
	log         []string
}

func newFake(caps Caps, files ...string) *fakeStore {
	s := &fakeStore{caps: caps, files: map[string]string{}}
	for _, f := range files {
		s.files[f] = "old"
	}
	return s
}

func (s *fakeStore) Caps() Caps { return s.caps }
func (s *fakeStore) Write(local, remote string, exclusive bool) error {
	s.log = append(s.log, fmt.Sprintf("write %s %v", remote, exclusive))
	if s.flaky > 0 {
		s.flaky--
		return &TemporaryError{Err: errors.New("connection reset")}
	}
	if _, ok := s.files[remote]; ok && exclusive {
		return ErrExists
	}
	s.files[remote] = local
	return nil
}
func (s *fakeStore) Rename(from, to string, replace bool) error {
	s.log = append(s.log, fmt.Sprintf("rename %s %s %v", from, to, replace))
	if s.breakRename {
		s.breakRename = false
		return errors.New("connection dropped")
	}
	if _, ok := s.files[to]; ok && !replace {
		return ErrExists
	}
	s.files[to] = s.files[from]
	delete(s.files, from)
	return nil
}
func (s *fakeStore) Exists(remote string) (bool, error) {
	s.log = append(s.log, "exists "+remote)
	_, ok := s.files[remote]
	return ok, nil
}
func (s *fakeStore) Delete(remote string) error { delete(s.files, remote); return nil }

var allCaps = Caps{CreateExclusive: true, RenameNoReplace: true}

func rules(ie IfExists, atomic bool) Rules {
	r := DefaultRules()
	r.IfExists, r.Atomic = ie, atomic
	return r
}

func run(s *fakeStore, r Rules) (Delivered, error) {
	return deliverWith(s, "new", "out/r.xlsx", r, func(time.Duration) {})
}

func TestLegacyAndAtomicDelivery(t *testing.T) {
	s := newFake(allCaps)
	if d, err := run(s, LegacyRules()); err != nil || d.Path != "out/r.xlsx" || fmt.Sprint(s.log) != "[write out/r.xlsx false]" {
		t.Fatalf("%v %v %v", d, err, s.log)
	}
	s = newFake(allCaps, "out/r.xlsx")
	if _, err := run(s, rules(IfExistsOverwrite, true)); err != nil || s.files["out/r.xlsx"] != "new" || len(s.files) != 1 {
		t.Fatalf("%v %v", err, s.files)
	}
	if s.log[0] != "write out/.r.xlsx.dre-part false" {
		t.Fatal(s.log)
	}
	// A dropped rename leaves nothing behind.
	s = newFake(allCaps)
	s.breakRename = true
	if _, err := run(s, rules(IfExistsOverwrite, true)); err == nil || len(s.files) != 0 {
		t.Fatalf("%v %v", err, s.files)
	}
	r := rules(IfExistsOverwrite, true)
	r.TempDir = "../staging"
	s = newFake(allCaps)
	run(s, r)
	if s.log[0] != "write out/../staging/.r.xlsx.dre-part false" {
		t.Fatal(s.log)
	}
}

func TestIfExists(t *testing.T) {
	for _, atomic := range []bool{false, true} {
		for _, caps := range []Caps{allCaps, {}} {
			s := newFake(caps, "out/r.xlsx")
			_, err := run(s, rules(IfExistsError, atomic))
			var e *Error
			if !errors.As(err, &e) || e.Code != "file-exists" || e.Kind != "delivery" || len(s.files) != 1 || s.files["out/r.xlsx"] != "old" {
				t.Fatalf("error atomic=%v caps=%v: %v %v", atomic, caps, err, s.files)
			}
			s = newFake(caps, "out/r.xlsx", "out/r_2.xlsx")
			d, err := run(s, rules(IfExistsNumber, atomic))
			if err != nil || d.Path != "out/r_3.xlsx" || s.files["out/r_3.xlsx"] != "new" || len(s.files) != 3 {
				t.Fatalf("number atomic=%v caps=%v: %v %v %v", atomic, caps, d, err, s.files)
			}
		}
	}
	s := newFake(Caps{CreateExclusive: true, VisibleWhenComplete: true})
	run(s, rules(IfExistsError, true))
	if fmt.Sprint(s.log) != "[write out/r.xlsx true]" {
		t.Fatal(s.log)
	}
}

func TestRetries(t *testing.T) {
	s := newFake(allCaps)
	s.flaky = 2
	var waits []time.Duration
	d, err := deliverWith(s, "new", "r.csv", DefaultRules(), func(w time.Duration) { waits = append(waits, w) })
	if err != nil || d.Attempts != 3 || len(waits) != 2 || waits[0] >= waits[1] {
		t.Fatalf("%v %v %v", d, err, waits)
	}
	s = newFake(allCaps)
	s.flaky = 9
	if _, err := run(s, DefaultRules()); err == nil || err.Error() != "connection reset (after 4 tries)" {
		t.Fatal(err)
	}
}

func TestRulesFromSettings(t *testing.T) {
	r, w, err := RulesFrom(DefaultRules(),
		map[string]any{"connect_timeout": "2m", "upload_timeout": 90.0, "retries": "0"},
		map[string]any{"if_exists": "number", "atomic": false, "temp_dir": "../in"},
		map[string]string{"upload_timeout": "timeout"})
	if err != nil || r.IfExists != IfExistsNumber || r.Atomic || r.TempDir != "../in" || r.Retries != 0 ||
		r.ConnectTimeout != 2*time.Minute || r.Timeout != 90*time.Second || fmt.Sprint(w) != "[`upload_timeout` is deprecated; use `timeout`]" {
		t.Fatalf("%+v %v %v", r, w, err)
	}
	if _, _, err := RulesFrom(DefaultRules(), nil, map[string]any{"if_exists": "keep"}, nil); err == nil {
		t.Fatal("bad if_exists accepted")
	}
	for in, want := range map[any]time.Duration{"30s": 30 * time.Second, "1500ms": 1500 * time.Millisecond, 45.0: 45 * time.Second, "1h": time.Hour} {
		if d, err := ParseDuration(in); err != nil || d != want {
			t.Fatalf("%v: %v %v", in, d, err)
		}
	}
	for _, in := range []any{"soon", "5 days", -1.0, true} {
		if _, err := ParseDuration(in); err == nil {
			t.Fatalf("%v accepted", in)
		}
	}
	if numbered("x.tar.gz", 2) != "x.tar_2.gz" || numbered("a.b/.env", 2) != "a.b/.env_2" {
		t.Fatal(numbered("x.tar.gz", 2))
	}
}

func TestRetryTriesAgainOnlyOnTemporaryErrors(t *testing.T) {
	var waits []time.Duration
	sleep := func(d time.Duration) { waits = append(waits, d) }
	takeAttempts()
	n := 0
	v, err := retryWith(3, "post", sleep, func() (string, error) {
		n++
		if n < 3 {
			return "", &TemporaryError{Err: errors.New("HTTP 503"), RetryAfter: 2 * time.Second}
		}
		return "ok", nil
	})
	if v != "ok" || err != nil || n != 3 || len(waits) != 2 || waits[0] != 2*time.Second {
		t.Fatal(v, err, n, waits)
	}
	if takeAttempts() != 3 || takeAttempts() != 1 {
		t.Fatal("attempts")
	}
	n = 0
	_, err = retryWith(3, "post", sleep, func() (string, error) { n++; return "", errors.New("HTTP 401") })
	if n != 1 || err.Error() != "HTTP 401" {
		t.Fatal(n, err)
	}
	n = 0
	_, err = retryWith(1, "post", sleep, func() (string, error) {
		n++
		return "", &TemporaryError{Err: errors.New("HTTP 503")}
	})
	if n != 2 || err.Error() != "HTTP 503" {
		t.Fatal(n, err)
	}
	if RetryAfter(" 7 ") != 7*time.Second || RetryAfter("Wed, 21 Oct") != 0 {
		t.Fatal("Retry-After")
	}
}
