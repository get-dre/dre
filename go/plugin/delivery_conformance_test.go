package plugin

// The delivery cases every SDK shares (crates/dre-protocol/conformance/delivery.json, copied to
// plugintest/conformance), against this module's delivery code with an in-memory store.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"
)

type deliveryCase struct {
	Name  string `json:"name"`
	Base  string `json:"base"`
	Store struct {
		CreateExclusive     bool `json:"create_exclusive"`
		RenameNoReplace     bool `json:"rename_no_replace"`
		VisibleWhenComplete bool `json:"visible_when_complete"`
	} `json:"store"`
	Existing   map[string]string `json:"existing"`
	Connection map[string]any    `json:"connection"`
	Options    map[string]any    `json:"options"`
	Faults     struct {
		Temporary   int     `json:"temporary"`
		RetryAfter  float64 `json:"retry_after"`
		BreakRename bool    `json:"break_rename"`
	} `json:"faults"`
	Remote     string `json:"remote"`
	Deliveries []struct {
		Remote string `json:"remote"`
		Path   string `json:"path"`
	} `json:"deliveries"`
	Expect *struct {
		Path         *string           `json:"path"`
		Attempts     *int              `json:"attempts"`
		Ops          []string          `json:"ops"`
		Files        map[string]string `json:"files"`
		Waits        []float64         `json:"waits"`
		WaitsBetween [][2]float64      `json:"waits_between"`
		SettingsErr  string            `json:"settings_error"`
		Error        *struct {
			Kind     string `json:"kind"`
			Code     string `json:"code"`
			Contains string `json:"contains"`
		} `json:"error"`
	} `json:"expect"`
}

// memoryStore logs every call and fails on cue; the local file's content is always "new".
type memoryStore struct {
	caps        Caps
	files       map[string]string
	temporary   int
	retryAfter  time.Duration
	breakRename bool
	ops         []string
}

func (s *memoryStore) Caps() Caps { return s.caps }
func (s *memoryStore) Write(_, remote string, exclusive bool) error {
	op := "write " + remote
	if exclusive {
		op += " exclusive"
	}
	s.ops = append(s.ops, op)
	if s.temporary > 0 {
		s.temporary--
		return &TemporaryError{Err: errors.New("connection reset"), RetryAfter: s.retryAfter}
	}
	if _, ok := s.files[remote]; ok && exclusive {
		return ErrExists
	}
	s.files[remote] = "new"
	return nil
}
func (s *memoryStore) Rename(from, to string, replace bool) error {
	op := "rename " + from + " " + to
	if replace {
		op += " replace"
	}
	s.ops = append(s.ops, op)
	if s.breakRename {
		s.breakRename = false
		return errors.New("connection dropped")
	}
	if _, ok := s.files[to]; ok && !replace {
		return ErrExists
	}
	v, ok := s.files[from]
	if !ok {
		return fmt.Errorf("no file at %s", from)
	}
	delete(s.files, from)
	s.files[to] = v
	return nil
}
func (s *memoryStore) Exists(remote string) (bool, error) {
	s.ops = append(s.ops, "exists "+remote)
	_, ok := s.files[remote]
	return ok, nil
}
func (s *memoryStore) Delete(remote string) error {
	s.ops = append(s.ops, "delete "+remote)
	delete(s.files, remote)
	return nil
}

func TestTheSDKFollowsEveryDeliveryCase(t *testing.T) {
	b, err := os.ReadFile(filepath.Join("plugintest", "conformance", "delivery.json"))
	if err != nil {
		t.Fatalf("the shared delivery cases: %v", err)
	}
	var file struct {
		Cases []deliveryCase `json:"cases"`
	}
	if err := json.Unmarshal(b, &file); err != nil {
		t.Fatal(err)
	}
	for _, c := range file.Cases {
		if err := runDeliveryCase(c); err != nil {
			t.Errorf("%s: %v", c.Name, err)
		}
	}
}

func runDeliveryCase(c deliveryCase) error {
	base := DefaultRules()
	if c.Base == "legacy" {
		base = LegacyRules()
	}
	rules, _, err := RulesFrom(base, c.Connection, c.Options, nil)
	x := c.Expect
	if err != nil {
		if x != nil && x.SettingsErr != "" && strings.Contains(err.Error(), x.SettingsErr) {
			return nil
		}
		return fmt.Errorf("the settings were refused: %v", err)
	}
	files := map[string]string{}
	for k, v := range c.Existing {
		files[k] = v
	}
	s := &memoryStore{
		caps:        Caps{CreateExclusive: c.Store.CreateExclusive, RenameNoReplace: c.Store.RenameNoReplace, VisibleWhenComplete: c.Store.VisibleWhenComplete},
		files:       files,
		temporary:   c.Faults.Temporary,
		retryAfter:  time.Duration(c.Faults.RetryAfter * float64(time.Second)),
		breakRename: c.Faults.BreakRename,
	}
	for _, l := range c.Deliveries {
		d, err := deliverWith(s, "new", l.Remote, rules, func(time.Duration) {})
		if err != nil || d.Path != l.Path {
			return fmt.Errorf("%s landed at %q (%v), expected %s", l.Remote, d.Path, err, l.Path)
		}
	}
	if c.Remote == "" || x == nil {
		return nil
	}
	if x.SettingsErr != "" {
		return fmt.Errorf("expected the settings to be refused (`%s`)", x.SettingsErr)
	}
	takeAttempts()
	var waits []float64
	d, err := deliverWith(s, "new", c.Remote, rules, func(w time.Duration) { waits = append(waits, w.Seconds()) })
	attempts := takeAttempts()
	switch {
	case err == nil && x.Error == nil:
		if x.Path != nil && d.Path != *x.Path {
			return fmt.Errorf("landed at %s, expected %s", d.Path, *x.Path)
		}
	case err != nil && x.Error != nil:
		var e *Error
		if !errors.As(err, &e) {
			return fmt.Errorf("a failed delivery must be an *Error with a kind, got %T: %v", err, err)
		}
		if x.Error.Kind != "" && e.Kind != x.Error.Kind {
			return fmt.Errorf("error kind %q, expected %q", e.Kind, x.Error.Kind)
		}
		if x.Error.Code != "" && e.Code != x.Error.Code {
			return fmt.Errorf("error code %q, expected %q", e.Code, x.Error.Code)
		}
		if !strings.Contains(e.Message, x.Error.Contains) {
			return fmt.Errorf("error %q doesn't contain %q", e.Message, x.Error.Contains)
		}
	case err == nil:
		return fmt.Errorf("expected an error, it landed at %s", d.Path)
	default:
		return fmt.Errorf("failed: %v", err)
	}
	if x.Attempts != nil && attempts != *x.Attempts {
		return fmt.Errorf("took %d tries, expected %d", attempts, *x.Attempts)
	}
	if x.Ops != nil && !reflect.DeepEqual(s.ops, x.Ops) {
		return fmt.Errorf("store calls %q, expected %q", s.ops, x.Ops)
	}
	if x.Files != nil && !reflect.DeepEqual(s.files, x.Files) {
		return fmt.Errorf("files %v, expected %v", s.files, x.Files)
	}
	if x.Waits != nil && !(len(waits) == 0 && len(x.Waits) == 0) && !reflect.DeepEqual(waits, x.Waits) {
		return fmt.Errorf("waited %vs, expected %vs", waits, x.Waits)
	}
	if x.WaitsBetween != nil {
		ok := len(waits) == len(x.WaitsBetween)
		for i := 0; ok && i < len(waits); i++ {
			ok = x.WaitsBetween[i][0] <= waits[i] && waits[i] <= x.WaitsBetween[i][1]
		}
		if !ok {
			return fmt.Errorf("waited %vs, expected within %v", waits, x.WaitsBetween)
		}
	}
	return nil
}
