//go:build !windows

package hostinfo

func windowsCPUModel() string    { return "" }
func windowsMemoryBytes() uint64 { return 0 }
