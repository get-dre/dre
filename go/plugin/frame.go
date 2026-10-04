package plugin

// Framing (docs/protocol.md): a 4-byte big-endian length, then a body whose first byte is the
// frame type: 'J' for one JSON control message, 'A' for one Arrow IPC stream.

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/ipc"
	"github.com/apache/arrow-go/v18/arrow/memory"
)

const (
	TagJSON  = 'J'
	TagArrow = 'A'
	maxFrame = 1 << 30
)

// ErrEOF means core closed stdin between frames: a clean end.
var ErrEOF = errors.New("core closed the connection")

type Frame struct {
	Tag  byte
	Body []byte // without the tag byte
}

func ReadFrame(r io.Reader) (Frame, error) {
	var head [4]byte
	n, err := io.ReadFull(r, head[:])
	if n == 0 && (err == io.EOF || err == io.ErrUnexpectedEOF) {
		return Frame{}, ErrEOF
	}
	if err != nil {
		return Frame{}, fmt.Errorf("truncated frame header: %w", err)
	}
	size := binary.BigEndian.Uint32(head[:])
	if size == 0 || size > maxFrame {
		return Frame{}, fmt.Errorf("bad frame length %d", size)
	}
	body := make([]byte, size)
	if _, err := io.ReadFull(r, body); err != nil {
		return Frame{}, fmt.Errorf("truncated frame body: %w", err)
	}
	switch body[0] {
	case TagJSON:
		if !json.Valid(body[1:]) {
			return Frame{}, errors.New("a JSON frame holds invalid JSON")
		}
		return Frame{Tag: TagJSON, Body: body[1:]}, nil
	case TagArrow:
		return Frame{Tag: TagArrow, Body: body[1:]}, nil
	default:
		return Frame{}, fmt.Errorf("unknown frame type byte 0x%02x", body[0])
	}
}

func WriteFrame(w *bufio.Writer, tag byte, body []byte) error {
	if len(body)+1 > maxFrame {
		return fmt.Errorf("frame of %d bytes is too large", len(body)+1)
	}
	var head [5]byte
	binary.BigEndian.PutUint32(head[:4], uint32(len(body)+1))
	head[4] = tag
	if _, err := w.Write(head[:]); err != nil {
		return err
	}
	if _, err := w.Write(body); err != nil {
		return err
	}
	return w.Flush()
}

func WriteJSON(w *bufio.Writer, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return WriteFrame(w, TagJSON, b)
}

// EncodeRecord writes one record as a self-contained IPC stream (schema, batch, end marker).
func EncodeRecord(rec arrow.Record) ([]byte, error) {
	var buf bytes.Buffer
	w := ipc.NewWriter(&buf, ipc.WithSchema(rec.Schema()), ipc.WithAllocator(memory.DefaultAllocator))
	if err := w.Write(rec); err != nil {
		return nil, err
	}
	if err := w.Close(); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// DecodeRecords reads every record of one IPC stream. The caller releases them.
func DecodeRecords(b []byte) (*arrow.Schema, []arrow.Record, error) {
	r, err := ipc.NewReader(bytes.NewReader(b), ipc.WithAllocator(memory.DefaultAllocator))
	if err != nil {
		return nil, nil, fmt.Errorf("bad Arrow data: %w", err)
	}
	defer r.Release()
	var recs []arrow.Record
	for r.Next() {
		rec := r.Record()
		rec.Retain()
		recs = append(recs, rec)
	}
	if err := r.Err(); err != nil {
		for _, rec := range recs {
			rec.Release()
		}
		return nil, nil, fmt.Errorf("bad Arrow data: %w", err)
	}
	return r.Schema(), recs, nil
}
