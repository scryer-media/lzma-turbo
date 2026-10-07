//go:build linux

package hostinfo

import "os"

// LoadAverage is the 1-minute load average and where it came from.
func LoadAverage() (float64, string) {
	data, err := os.ReadFile("/proc/loadavg")
	if err != nil {
		return -1, "unavailable"
	}
	if value, ok := parseLoadAverage(string(data)); ok {
		return value, "/proc/loadavg"
	}
	return -1, "unavailable"
}
