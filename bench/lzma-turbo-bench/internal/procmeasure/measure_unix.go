//go:build !windows

package procmeasure

import (
	"os"
	"os/exec"
	"runtime"
	"syscall"
)

type probe struct{}

func attachProbe(*exec.Cmd) probe { return probe{} }

func (probe) finish(*Measurement) {}

func fillRusage(measurement *Measurement, state *os.ProcessState) {
	usage, ok := state.SysUsage().(*syscall.Rusage)
	if !ok || usage == nil {
		return
	}
	maxRSS := int64(usage.Maxrss)
	if runtime.GOOS != "darwin" {
		// Linux and the BSDs report KiB; Darwin reports bytes.
		maxRSS *= 1024
	}
	measurement.MaxRSSBytes = maxRSS
	measurement.BlockInOps = int64(usage.Inblock)
	measurement.BlockOutOps = int64(usage.Oublock)
}

// configureKill puts the child in its own process group and makes the context
// cancel kill that whole group, so a timed-out run takes its descendants with
// it.
func configureKill(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error {
		if cmd.Process == nil {
			return nil
		}
		if err := syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL); err != nil {
			return cmd.Process.Kill()
		}
		return nil
	}
}

// NativeRSSSource is where Run reads the peak resident set here.
const NativeRSSSource = RSSSourceRusage
