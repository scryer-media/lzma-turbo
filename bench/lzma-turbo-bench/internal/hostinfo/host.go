// Package hostinfo describes the machine a run happened on: what a reader of
// a merged, many-host report needs to tell one row from another. It records
// no host name; the fleet runner names a host with --machine.
package hostinfo

import (
	"context"
	"os"
	"os/exec"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"

	"golang.org/x/sys/cpu"
)

// Host is the descriptor every report carries.
type Host struct {
	// Label is the operator's name for the host (--machine), or
	// "<os>-<arch>-<cpus>" when none was given.
	Label        string `json:"label"`
	OS           string `json:"os"`
	Kernel       string `json:"kernel"`
	Architecture string `json:"architecture"`
	CPU          string `json:"cpu"`
	CPUCount     int    `json:"cpu_count"`
	MemoryBytes  uint64 `json:"memory_bytes,omitempty"`
	// ISA is the instruction-set extensions this harness found, by the names
	// the crate's dispatch and its docs use (avx2, avx512vbmi2, gfni, neon,
	// sve2, ...), sorted.
	ISA []string `json:"isa"`
	// ISASource says where ISA came from.
	ISASource string `json:"isa_source"`
	// Tier is a one-word summary of the widest vector ISA present:
	// "avx512", "avx2", "sse", "sve2", "sve", "neon" or "baseline".
	Tier string `json:"tier"`
	// LoadSource says how the per-run load average was read.
	LoadSource string `json:"load_source"`
}

// Collect describes this machine.
func Collect(ctx context.Context, label string) Host {
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	host := Host{
		Label:        label,
		OS:           runtime.GOOS,
		Architecture: runtime.GOARCH,
		CPUCount:     runtime.NumCPU(),
	}
	if host.Label == "" {
		host.Label = runtime.GOOS + "-" + runtime.GOARCH + "-" + strconv.Itoa(host.CPUCount)
	}
	host.Kernel = kernel(ctx)
	host.CPU = cpuModel(ctx)
	host.MemoryBytes = memoryBytes(ctx)
	host.ISA, host.ISASource = isa()
	host.Tier = tier(host.ISA)
	_, host.LoadSource = LoadAverage()
	return host
}

func kernel(ctx context.Context) string {
	if runtime.GOOS == "windows" {
		if value := commandLine(ctx, "cmd", "/c", "ver"); value != "" {
			return value
		}
		return "not-collected"
	}
	if value := commandLine(ctx, "uname", "-sr"); value != "" {
		return value
	}
	return "not-collected"
}

// Arm's part numbers for the cores a cloud fleet runs, for when lscpu is
// absent and /proc/cpuinfo has only the numbers.
var armParts = map[string]string{
	"0xd0c": "Neoverse-N1",
	"0xd40": "Neoverse-V1",
	"0xd49": "Neoverse-N2",
	"0xd4f": "Neoverse-V2",
	"0xd8e": "Neoverse-N3",
	"0xd84": "Neoverse-V3",
}

func cpuModel(ctx context.Context) string {
	switch runtime.GOOS {
	case "darwin":
		if value := commandLine(ctx, "sysctl", "-n", "machdep.cpu.brand_string"); value != "" {
			return value
		}
	case "linux":
		if out := commandLine(ctx, "lscpu"); out != "" {
			for _, line := range strings.Split(out, "\n") {
				if name, value, ok := strings.Cut(line, ":"); ok && strings.TrimSpace(name) == "Model name" {
					if value = strings.TrimSpace(value); value != "" && value != "-" {
						return value
					}
				}
			}
		}
		if data, err := os.ReadFile("/proc/cpuinfo"); err == nil {
			part := ""
			for _, line := range strings.Split(string(data), "\n") {
				name, value, ok := strings.Cut(line, ":")
				if !ok {
					continue
				}
				switch strings.TrimSpace(name) {
				case "model name":
					return strings.TrimSpace(value)
				case "CPU part":
					if part == "" {
						part = strings.TrimSpace(value)
					}
				}
			}
			if model, ok := armParts[part]; ok {
				return model
			}
			if part != "" {
				return "arm part " + part
			}
		}
	case "windows":
		if value := windowsCPUModel(); value != "" {
			return value
		}
	}
	return runtime.GOARCH
}

func memoryBytes(ctx context.Context) uint64 {
	switch runtime.GOOS {
	case "darwin":
		if value, err := strconv.ParseUint(commandLine(ctx, "sysctl", "-n", "hw.memsize"), 10, 64); err == nil {
			return value
		}
	case "linux":
		if data, err := os.ReadFile("/proc/meminfo"); err == nil {
			for _, line := range strings.Split(string(data), "\n") {
				fields := strings.Fields(line)
				if len(fields) >= 2 && fields[0] == "MemTotal:" {
					if value, err := strconv.ParseUint(fields[1], 10, 64); err == nil {
						return value * 1024
					}
				}
			}
		}
	case "windows":
		return windowsMemoryBytes()
	}
	return 0
}

// isa reads the extensions from golang.org/x/sys/cpu, which asks CPUID on
// x86 (every OS), the auxiliary vector on Linux arm64, sysctl on macOS and
// IsProcessorFeaturePresent on Windows; on Linux the kernel's own flag list
// adds the few x86 extensions x/sys/cpu does not name.
func isa() ([]string, string) {
	set := map[string]bool{}
	add := func(name string, on bool) {
		if on {
			set[name] = true
		}
	}
	source := "golang.org/x/sys/cpu"
	switch runtime.GOARCH {
	case "amd64", "386":
		x := cpu.X86
		add("sse4.2", x.HasSSE42)
		add("pclmulqdq", x.HasPCLMULQDQ)
		add("aes", x.HasAES)
		add("avx", x.HasAVX)
		add("avx2", x.HasAVX2)
		add("bmi2", x.HasBMI2)
		add("avx512f", x.HasAVX512F)
		add("avx512bw", x.HasAVX512BW)
		add("avx512vl", x.HasAVX512VL)
		add("avx512vbmi", x.HasAVX512VBMI)
		add("avx512vbmi2", x.HasAVX512VBMI2)
		add("vpclmulqdq", x.HasAVX512VPCLMULQDQ)
		add("gfni", x.HasAVX512GFNI)
		add("vaes", x.HasAVX512VAES)
	case "arm64":
		a := cpu.ARM64
		add("neon", a.HasASIMD)
		add("pmull", a.HasPMULL)
		add("crc32", a.HasCRC32)
		add("sha2", a.HasSHA2)
		add("sha3", a.HasSHA3)
		add("sha512", a.HasSHA512)
		add("atomics", a.HasATOMICS)
		add("i8mm", a.HasI8MM)
		add("sve", a.HasSVE)
		add("sve2", a.HasSVE2)
	}
	if runtime.GOOS == "linux" {
		if data, err := os.ReadFile("/proc/cpuinfo"); err == nil {
			flags := map[string]string{
				"sha_ni": "sha-ni", "gfni": "gfni", "vaes": "vaes", "vpclmulqdq": "vpclmulqdq",
				"avx512_vbmi2": "avx512vbmi2", "sve": "sve", "sve2": "sve2", "asimd": "neon",
			}
			for _, line := range strings.Split(string(data), "\n") {
				name, value, ok := strings.Cut(line, ":")
				if !ok {
					continue
				}
				if key := strings.TrimSpace(name); key != "flags" && key != "Features" {
					continue
				}
				for _, flag := range strings.Fields(value) {
					if mapped, ok := flags[flag]; ok {
						set[mapped] = true
					}
				}
				source += " + /proc/cpuinfo"
				break
			}
		}
	}
	names := make([]string, 0, len(set))
	for name := range set {
		names = append(names, name)
	}
	sort.Strings(names)
	return names, source
}

func tier(isa []string) string {
	has := map[string]bool{}
	for _, name := range isa {
		has[name] = true
	}
	switch {
	case has["avx512f"] && has["avx512bw"]:
		return "avx512"
	case has["avx2"]:
		return "avx2"
	case has["sse4.2"]:
		return "sse"
	case has["sve2"]:
		return "sve2"
	case has["sve"]:
		return "sve"
	case has["neon"]:
		return "neon"
	}
	return "baseline"
}

func commandLine(ctx context.Context, program string, args ...string) string {
	output, err := exec.CommandContext(ctx, program, args...).Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(output))
}

// parseLoadAverage reads the first number of a load-average line.
func parseLoadAverage(text string) (float64, bool) {
	fields := strings.Fields(strings.Trim(strings.TrimSpace(text), "{} "))
	if len(fields) == 0 {
		return 0, false
	}
	value, err := strconv.ParseFloat(fields[0], 64)
	return value, err == nil
}
