package plugin

// SetMaxRecord lowers the largest Arrow batch a frame holds, for a test; restore puts it back.
func SetMaxRecord(n int) (restore func()) {
	old := maxRecord
	maxRecord = n
	return func() { maxRecord = old }
}
