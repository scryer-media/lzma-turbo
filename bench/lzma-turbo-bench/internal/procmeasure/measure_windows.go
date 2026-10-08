//go:build windows

package procmeasure

import (
	"os"
	"os/exec"
	"syscall"
	"unsafe"
)

var (
	kernel32                    = syscall.NewLazyDLL("kernel32.dll")
	procGetProcessIoCounters    = kernel32.NewProc("GetProcessIoCounters")
	procK32GetProcessMemoryInfo = kernel32.NewProc("K32GetProcessMemoryInfo")
)

const (
	processQueryInformation = 0x0400
	processVMRead           = 0x0010
)

type ioCounters struct {
	ReadOperationCount  uint64
	WriteOperationCount uint64
	OtherOperationCount uint64
	ReadTransferCount   uint64
	WriteTransferCount  uint64
	OtherTransferCount  uint64
}

type processMemoryCounters struct {
	CB                         uint32
	PageFaultCount             uint32
	PeakWorkingSetSize         uintptr
	WorkingSetSize             uintptr
	QuotaPeakPagedPoolUsage    uintptr
	QuotaPagedPoolUsage        uintptr
	QuotaPeakNonPagedPoolUsage uintptr
	QuotaNonPagedPoolUsage     uintptr
	PagefileUsage              uintptr
	PeakPagefileUsage          uintptr
}

// probe holds a process handle taken right after start. The process object
// stays queryable after exit for as long as the handle is open, which is how
// the peak working set of an exited child is read (Process.PeakWorkingSet64
// reads null once the process has gone).
type probe struct{ handle syscall.Handle }

func attachProbe(cmd *exec.Cmd) probe {
	if cmd.Process == nil {
		return probe{}
	}
	handle, err := syscall.OpenProcess(processQueryInformation|processVMRead, false, uint32(cmd.Process.Pid))
	if err != nil {
		return probe{}
	}
	return probe{handle: handle}
}

func (p probe) finish(measurement *Measurement) {
	if p.handle == 0 {
		return
	}
	defer syscall.CloseHandle(p.handle)
	var counters ioCounters
	if r, _, _ := procGetProcessIoCounters.Call(uintptr(p.handle), uintptr(unsafe.Pointer(&counters))); r != 0 {
		measurement.ReadOps = int64(counters.ReadOperationCount)
		measurement.WriteOps = int64(counters.WriteOperationCount)
		measurement.ReadBytes = int64(counters.ReadTransferCount)
		measurement.WriteBytes = int64(counters.WriteTransferCount)
	}
	var memory processMemoryCounters
	memory.CB = uint32(unsafe.Sizeof(memory))
	if r, _, _ := procK32GetProcessMemoryInfo.Call(uintptr(p.handle), uintptr(unsafe.Pointer(&memory)), uintptr(memory.CB)); r != 0 {
		measurement.MaxRSSBytes = int64(memory.PeakWorkingSetSize)
	}
}

func fillRusage(*Measurement, *os.ProcessState) {}

// configureKill leaves the default kill of the direct child; WaitDelay closes
// the pipes so a surviving grandchild cannot hold Wait open.
func configureKill(*exec.Cmd) {}

// NativeRSSSource is where Run reads the peak resident set here.
const NativeRSSSource = RSSSourcePeakWorkingSet
