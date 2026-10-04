package main

// The connector returns Arrow Go v12 batches; the shared protocol module uses arrow-go v18.
// Each batch crosses over as an Arrow IPC stream, which both versions read and write: a copy in
// memory, small next to fetching the batch from the warehouse.

import (
	"bytes"
	"fmt"
	"io"

	"github.com/apache/arrow-go/v18/arrow"
	"github.com/apache/arrow-go/v18/arrow/ipc"
	"github.com/apache/arrow-go/v18/arrow/memory"
	arrow12 "github.com/apache/arrow/go/v12/arrow"
	ipc12 "github.com/apache/arrow/go/v12/arrow/ipc"
	memory12 "github.com/apache/arrow/go/v12/arrow/memory"
	dbsqlrows "github.com/databricks/databricks-sql-go/rows"
)

// bridged is the connector's batch iterator as a plugin.Result.
type bridged struct {
	it dbsqlrows.ArrowBatchIterator
}

func (b *bridged) Schema() (*arrow.Schema, error) {
	s, err := b.it.Schema()
	if err != nil {
		return nil, cleanErr(err)
	}
	return schemaV18(s)
}

func (b *bridged) HasNext() bool { return b.it.HasNext() }

func (b *bridged) Next() (arrow.Record, error) {
	rec, err := b.it.Next()
	if err == io.EOF {
		return nil, err
	}
	if err != nil {
		return nil, cleanErr(err)
	}
	defer rec.Release()
	return recordV18(rec)
}

// recordV18 copies a v12 record into a v18 one. The caller releases it.
func recordV18(rec arrow12.Record) (arrow.Record, error) {
	var buf bytes.Buffer
	w := ipc12.NewWriter(&buf, ipc12.WithSchema(rec.Schema()), ipc12.WithAllocator(memory12.DefaultAllocator))
	if err := w.Write(rec); err != nil {
		return nil, fmt.Errorf("can't pass on a result batch: %w", err)
	}
	if err := w.Close(); err != nil {
		return nil, err
	}
	r, err := ipc.NewReader(&buf, ipc.WithAllocator(memory.DefaultAllocator))
	if err != nil {
		return nil, fmt.Errorf("can't pass on a result batch: %w", err)
	}
	defer r.Release()
	if !r.Next() {
		if err := r.Err(); err != nil {
			return nil, err
		}
		return nil, fmt.Errorf("can't pass on a result batch: it was empty")
	}
	out := r.Record()
	out.Retain()
	return out, nil
}

// schemaV18 is a v12 schema as a v18 one.
func schemaV18(s *arrow12.Schema) (*arrow.Schema, error) {
	var buf bytes.Buffer
	w := ipc12.NewWriter(&buf, ipc12.WithSchema(s))
	if err := w.Close(); err != nil {
		return nil, err
	}
	r, err := ipc.NewReader(&buf)
	if err != nil {
		return nil, fmt.Errorf("can't read the result's columns: %w", err)
	}
	defer r.Release()
	return r.Schema(), nil
}
