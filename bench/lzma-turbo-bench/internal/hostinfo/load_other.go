//go:build !linux && !windows

package hostinfo

import "os/exec"

// LoadAverage is the 1-minute load average from sysctl vm.loadavg on macOS
// and the BSDs, and where it came from.
func LoadAverage() (float64, string) {
	output, err := exec.Command("sysctl", "-n", "vm.loadavg").Output()
	if err != nil {
		return -1, "unavailable"
	}
	if value, ok := parseLoadAverage(string(output)); ok {
		return value, "sysctl vm.loadavg"
	}
	return -1, "unavailable"
}
