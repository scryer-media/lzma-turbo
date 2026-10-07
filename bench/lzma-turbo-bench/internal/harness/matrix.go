package harness

import (
	"fmt"
	"path"
	"runtime"
	"strconv"
	"strings"
)

// Contender roles.
const (
	RoleOurs      = "ours"
	RoleReference = "reference"
)

// Contender kinds: which program a run launches.
const (
	KindShot  = "shot"  // lzma-bench --shot (lzma-turbo, lzma-rust2, liblzma)
	KindXZ    = "xz"    // XZ Utils
	Kind7zz   = "7zz"   // 7-Zip
	Kind7lzma = "7lzma" // the LZMA SDK's command-line coder
)

// Stdout handling for a contender.
const (
	StdoutDiscard = ""      // os.DevNull, or nothing written (7zz t)
	StdoutCount   = "count" // piped and counted: an encoder's output size
	StdoutLine    = "line"  // captured: a shot's JSON line
)

// Contender is one side of a scenario.
type Contender struct {
	Name string `json:"name"`
	Role string `json:"role"`
	Kind string `json:"kind"`
	// Args follow the program; the input path is where "{in}" stands, and
	// the null device where "{null}" does.
	Args   []string `json:"args"`
	Stdout string   `json:"-"`
}

// Scenario is one row of the matrix.
type Scenario struct {
	ID    string `json:"scenario"`
	Group string `json:"group"`
	Title string `json:"title"`
	// Input is the fixture every contender reads.
	Input     string `json:"input"`
	Threads   int    `json:"threads,omitempty"`
	Preset    int    `json:"preset,omitempty"`
	Filter    string `json:"filter,omitempty"`
	Direction string `json:"direction"`
	// Source is the uncompressed fixture: its CRC-32 is what a decode
	// shot's output must have, Repeat times over (multi.xz is three copies),
	// and its size is the bytes a throughput is quoted on.
	Source string `json:"source,omitempty"`
	Repeat int    `json:"repeat,omitempty"`
	// Metric is "wall" (the process, the default) or "inproc" (the timed
	// region a shot reports: the bare converters, which no oracle has).
	Metric     string      `json:"metric"`
	Size       string      `json:"size"` // p256 | 1g
	Contenders []Contender `json:"contenders"`
	// Skip is set when the matrix already knows the host cannot run it.
	Skip string `json:"skip,omitempty"`
	// NoReference says why a scenario has only lzma-turbo.
	NoReference string `json:"no_reference,omitempty"`
}

// Ours is the lzma-turbo contender.
func (s Scenario) Ours() Contender {
	for _, c := range s.Contenders {
		if c.Role == RoleOurs {
			return c
		}
	}
	return Contender{}
}

// MatrixOptions selects the matrix.
type MatrixOptions struct {
	Quick     bool
	Threads   []int // 0 means every logical CPU ("all")
	Presets   []int
	MTPresets []int
	Sizes     []string // p256, 1g
	Only      []string
	CPUs      int
}

// DefaultMatrixOptions are the brief's sweeps; quick trims them to a smoke.
func DefaultMatrixOptions(quick bool) MatrixOptions {
	if quick {
		return MatrixOptions{Quick: true, Threads: []int{1, 0}, Presets: []int{1, 5}, MTPresets: []int{5}, Sizes: []string{"p256"}}
	}
	return MatrixOptions{Threads: []int{1, 2, 4, 8, 16, 0}, Presets: []int{1, 3, 5, 6, 9}, MTPresets: []int{5, 6}, Sizes: []string{"p256", "1g"}}
}

// ParseInts reads "1,2,all" style lists; "all" is 0.
func ParseInts(text string) ([]int, error) {
	var out []int
	for _, part := range strings.Split(text, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		if part == "all" {
			out = append(out, 0)
			continue
		}
		n, err := strconv.Atoi(part)
		if err != nil || n < 0 {
			return nil, fmt.Errorf("%q is not a count or \"all\"", part)
		}
		out = append(out, n)
	}
	return out, nil
}

func shot(name, role string, args ...string) Contender {
	return Contender{Name: name, Role: role, Kind: KindShot, Args: append(args, "{in}"), Stdout: StdoutLine}
}

func xzDecode(args ...string) Contender {
	all := append([]string{"-dc"}, args...)
	return Contender{Name: "xz", Role: RoleReference, Kind: KindXZ, Args: append(all, "{in}")}
}

func sevenTest(threads int) Contender {
	return Contender{Name: "7zz", Role: RoleReference, Kind: Kind7zz,
		Args: []string{"t", "-bso0", "-bsp0", "-mmt=" + strconv.Itoa(threads), "{in}"}}
}

func xzEncode(args ...string) Contender {
	all := append(append([]string{}, args...), "-c", "{in}")
	return Contender{Name: "xz", Role: RoleReference, Kind: KindXZ, Args: all, Stdout: StdoutCount}
}

// threadLabel is "t4", or "tall" for every logical CPU.
func threadLabel(requested int) string {
	if requested == 0 {
		return "tall"
	}
	return "t" + strconv.Itoa(requested)
}

// BuildMatrix lays out every scenario the options select, in run order.
func BuildMatrix(options MatrixOptions) []Scenario {
	cpus := options.CPUs
	if cpus <= 0 {
		cpus = runtime.NumCPU()
	}
	resolve := func(requested int) int {
		if requested == 0 {
			return cpus
		}
		return requested
	}
	// A sweep point beyond the host's CPUs is skipped, not oversubscribed:
	// it would time the scheduler, not the codec.
	threadSkip := func(requested int) string {
		if requested > cpus {
			return fmt.Sprintf("host has %d logical CPUs, fewer than %d threads", cpus, requested)
		}
		return ""
	}
	// Deduplicate the sweep after "all" resolves (all = 16 on a 16-CPU host).
	var threads []int
	seen := map[int]bool{}
	for _, t := range options.Threads {
		if r := resolve(t); !seen[r] {
			seen[r] = true
			threads = append(threads, t)
		}
	}
	sizes := map[string]bool{}
	for _, s := range options.Sizes {
		sizes[s] = true
	}

	var out []Scenario
	add := func(s Scenario) {
		if s.Metric == "" {
			s.Metric = "wall"
		}
		if s.Size == "" {
			s.Size = "p256"
		}
		if !sizes[s.Size] {
			return
		}
		if !selected(options.Only, s.ID) {
			return
		}
		out = append(out, s)
	}
	type sized struct{ size, label, lzma, source, sevenST, sevenMT string }
	both := []sized{
		{"p256", "p256", "p256.bin.lzma", "p256.bin", "p256.st.7z", "p256.mt.7z"},
		{"1g", "1g", "payload.bin.lzma", "payload.bin", "st.7z", "mt.7z"},
	}

	// --- LZMA1 -----------------------------------------------------------
	for _, s := range both {
		add(Scenario{ID: "decode/lzma1/" + s.label, Group: "decode-lzma1", Title: "LZMA1 (.lzma) decode, one thread",
			Input: s.lzma, Source: s.source, Direction: "decode", Threads: 1, Size: s.size,
			Contenders: []Contender{
				shot("lzma-turbo", RoleOurs, "--shot", "lzma1"),
				sevenTest(1),
				xzDecode("-T1", "--format=lzma"),
				{Name: "7lzma", Role: RoleReference, Kind: Kind7lzma, Args: []string{"d", "{in}", "{null}"}},
			}})
	}

	// --- LZMA2 in .xz, single stream --------------------------------------
	add(Scenario{ID: "decode/xz/p256", Group: "decode-xz", Title: ".xz single-block LZMA2 decode, one thread",
		Input: "p256.bin.xz", Source: "p256.bin", Direction: "decode", Threads: 1,
		Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz"), xzDecode("-T1"), sevenTest(1)}})
	add(Scenario{ID: "decode/lzma2-raw/p256", Group: "decode-xz", Title: "the bare LZMA2 stream of p256.bin.xz (Lzma2Reader, no container)",
		Input: "p256.bin.xz", Source: "p256.bin", Direction: "decode", Threads: 1,
		Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "lzma2"), xzDecode("-T1")}})
	add(Scenario{ID: "decode/xz-multi/p256x3", Group: "decode-xz", Title: "three concatenated .xz streams",
		Input: "multi.xz", Source: "p256.bin", Repeat: 3, Direction: "decode", Threads: 1,
		Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz"), xzDecode("-T1")}})

	// --- LZMA2 in .7z -----------------------------------------------------
	for _, s := range both {
		add(Scenario{ID: "decode/7z-st/" + s.label, Group: "decode-7z", Title: "single-stream .7z LZMA2 decode, one thread",
			Input: s.sevenST, Source: s.source, Direction: "decode", Threads: 1, Size: s.size,
			Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "lzma2"), sevenTest(1)}})
		// A single stream has no independent chunks, so the parallel
		// decoder has to fall back to one; this row prices that fallback.
		add(Scenario{ID: "decode/7z-st-fallback/" + s.label + "/tall", Group: "decode-7z",
			Title: "single-stream .7z through the parallel decoder at every CPU (fallback cost)",
			Input: s.sevenST, Source: s.source, Direction: "decode", Threads: cpus, Size: s.size,
			Contenders: []Contender{
				shot("lzma-turbo", RoleOurs, "--shot", "lzma2-mt", "--threads", strconv.Itoa(cpus)),
				sevenTest(cpus),
			}})
		for _, t := range threads {
			n := resolve(t)
			add(Scenario{ID: "decode/7z-mt/" + s.label + "/" + threadLabel(t), Group: "decode-7z-mt",
				Title: fmt.Sprintf("multi-chunk .7z LZMA2 decode, %d threads", n),
				Input: s.sevenMT, Source: s.source, Direction: "decode", Threads: n, Size: s.size, Skip: threadSkip(t),
				Contenders: []Contender{
					shot("lzma-turbo", RoleOurs, "--shot", "lzma2-mt", "--threads", strconv.Itoa(n)),
					sevenTest(n),
					shot("lzma-rust2", RoleReference, "--shot", "rust2-mt", "--threads", strconv.Itoa(n)),
				}})
		}
	}

	// --- parallel .xz -----------------------------------------------------
	type xzPar struct{ size, label, input, source string }
	for _, x := range []xzPar{
		{"p256", "p256.t8", "p256.t8.xz", "p256.bin"},
		{"p256", "p256.b16", "p256.b16.xz", "p256.bin"},
		{"1g", "1g.t8", "payload.t8.xz", "payload.bin"},
	} {
		for _, t := range threads {
			n := resolve(t)
			add(Scenario{ID: "decode/xz-par/" + x.label + "/" + threadLabel(t), Group: "decode-xz-par",
				Title: fmt.Sprintf("multi-block .xz, XzParallelReader, %d threads", n),
				Input: x.input, Source: x.source, Direction: "decode", Threads: n, Size: x.size, Skip: threadSkip(t),
				Contenders: []Contender{
					shot("lzma-turbo", RoleOurs, "--shot", "xz-par", "--threads", strconv.Itoa(n)),
					xzDecode("-T" + strconv.Itoa(n)),
					shot("liblzma", RoleReference, "--shot", "liblzma", "--threads", strconv.Itoa(n)),
				}})
		}
	}

	// --- encode -----------------------------------------------------------
	for _, p := range options.Presets {
		ps := strconv.Itoa(p)
		add(Scenario{ID: "encode/st/p" + ps, Group: "encode", Title: fmt.Sprintf(".xz encode, preset %d, one thread", p),
			Input: "p256.bin", Source: "p256.bin", Direction: "encode", Threads: 1, Preset: p,
			Contenders: []Contender{
				shot("lzma-turbo", RoleOurs, "--shot", "encode", "--preset", ps, "--threads", "1"),
				xzEncode("-T1", "-"+ps),
			}})
	}
	for _, p := range options.MTPresets {
		ps := strconv.Itoa(p)
		for _, t := range threads {
			n := resolve(t)
			if n == 1 {
				continue
			}
			add(Scenario{ID: "encode/mt/p" + ps + "/" + threadLabel(t), Group: "encode-mt",
				Title: fmt.Sprintf("multi-threaded .xz encode, preset %d, %d threads", p, n),
				Input: "p256.bin", Source: "p256.bin", Direction: "encode", Threads: n, Preset: p, Skip: threadSkip(t),
				Contenders: []Contender{
					shot("lzma-turbo", RoleOurs, "--shot", "encode", "--preset", ps, "--threads", strconv.Itoa(n)),
					xzEncode("-T"+strconv.Itoa(n), "-"+ps),
				}})
		}
	}

	// --- filters ----------------------------------------------------------
	type filterCase struct{ name, decodeInput, source string }
	cases := []filterCase{
		{"x86", "bcj-x86.code.xz", "codebin.bin"},
		{"arm64", "bcj-arm64.code.xz", "codebin.bin"},
	}
	for _, f := range bcjExtras {
		cases = append(cases, filterCase{f, "bcj-" + f + ".code.xz", "codebin.bin"})
	}
	cases = append(cases, filterCase{"delta:4", "delta.xz", "p256.bin"}, filterCase{"delta:64", "delta64.xz", "p256.bin"})
	for _, f := range cases {
		label := strings.ReplaceAll(f.name, ":", "")
		add(Scenario{ID: "filter/decode/" + label, Group: "filter", Title: "filter chain decode: " + f.name + " + LZMA2 in .xz",
			Input: f.decodeInput, Source: f.source, Direction: "decode", Threads: 1, Filter: f.name,
			Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz"), xzDecode("-T1")}})
		add(Scenario{ID: "filter/encode/" + label, Group: "filter", Title: "filter chain encode: " + f.name + " + LZMA2 preset 1",
			Input: f.source, Source: f.source, Direction: "encode", Threads: 1, Preset: 1, Filter: f.name,
			Contenders: []Contender{
				shot("lzma-turbo", RoleOurs, "--shot", "encode", "--preset", "1", "--threads", "1", "--filter", f.name),
				xzEncode("-T1", XZFilterOption(f.name), "--lzma2=preset=1"),
			}})
	}
	bare := []filterCase{}
	for _, f := range cases {
		bare = append(bare, filterCase{f.name, "", f.source})
	}
	bare = append(bare, filterCase{"bcj2", "", "codebin.bin"})
	for _, f := range bare {
		label := strings.ReplaceAll(f.name, ":", "")
		why := "no oracle runs a bare converter; xz applies filters only inside an .xz chain (the filter/encode and filter/decode rows)"
		if f.name == "bcj2" {
			why = "xz has no BCJ2, and 7-Zip runs it only as a four-stream 7z coder chain, which is not a bare converter"
		}
		for _, dir := range []string{"encode", "decode"} {
			add(Scenario{ID: "filter/raw/" + label + "/" + dir, Group: "filter-raw",
				Title: "bare converter, " + f.name + " " + dir + " in memory", Input: f.source, Source: f.source,
				Direction: dir, Threads: 1, Filter: f.name, Metric: "inproc", NoReference: why,
				Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "filter", "--filter", f.name, "--direction", dir)}})
		}
	}

	// --- checks -----------------------------------------------------------
	for _, c := range []struct{ name, input string }{
		{"none", "p256.none.xz"}, {"crc32", "p256.crc32.xz"}, {"crc64", "p256.bin.xz"}, {"sha256", "p256.sha256.xz"},
	} {
		add(Scenario{ID: "check/" + c.name, Group: "check", Title: ".xz decode with check " + c.name,
			Input: c.input, Source: "p256.bin", Direction: "decode", Threads: 1, Filter: "",
			Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz"), xzDecode("-T1")}})
	}
	return out
}

// selected reports whether an ID matches --only: each pattern is a path
// glob or a prefix ("decode/7z-mt" selects the whole sweep).
func selected(patterns []string, id string) bool {
	if len(patterns) == 0 {
		return true
	}
	for _, p := range patterns {
		if p == "" {
			continue
		}
		if strings.HasPrefix(id, p) {
			return true
		}
		if ok, _ := path.Match(p, id); ok {
			return true
		}
	}
	return false
}
