package harness

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/hostinfo"
	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/procmeasure"
)

// ReportSchema identifies report.json.
const ReportSchema = "lzma-turbo-bench/report"

// The orientation statements, in report.json and at the top of report.md.
const (
	RatioOrientation = "Every ratio = reference / lzma-turbo, >1 = lzma-turbo better: speed and CPU from median seconds (faster, less CPU), RSS from median peaks (a lower peak), size from output bytes (a smaller stream)."
	RarparNote       = "rarpar-bench orients its own ratios the other way, rarpar/reference with below 1.000 meaning rarpar is better; this harness inverts every ratio, RSS and size included, so above 1.000 always means lzma-turbo is better."
)

// Orientation records how every ratio reads.
type Orientation struct {
	// Ratio is the one direction every ratio in the report shares.
	Ratio       string `json:"ratio"`
	RarparBench string `json:"rarpar_bench"`
}

// ContenderSummary is one contender's measured runs in one scenario.
type ContenderSummary struct {
	Name    string `json:"name"`
	Role    string `json:"role"`
	Status  string `json:"status"`
	Reason  string `json:"reason,omitempty"`
	Command string `json:"command"`
	// Seconds is the scenario's metric: wall for a process, the shot's
	// timed region for an in-process row.
	Seconds     Stat   `json:"seconds"`
	Wall        Stat   `json:"wall_seconds"`
	CPU         Stat   `json:"cpu_seconds"`
	MaxRSS      Stat   `json:"max_rss"`
	MaxRSSBytes int64  `json:"max_rss_bytes"`
	RSSSource   string `json:"rss_source"`
	// PeakAllocBytes is the largest in-process allocation high-water mark a
	// shot saw (Rust heap only; liblzma's C allocations are not counted).
	PeakAllocBytes     int64   `json:"peak_alloc_bytes,omitempty"`
	InputBufferedBytes int64   `json:"input_buffered_bytes,omitempty"`
	BytesIn            int64   `json:"bytes_in,omitempty"`
	BytesOut           int64   `json:"bytes_out,omitempty"`
	MiBPerSecond       float64 `json:"mib_per_second,omitempty"`
	Runs               int     `json:"runs"`
}

// Ratio compares lzma-turbo with one reference.
type Ratio struct {
	Reference string   `json:"reference"`
	Speedup   *float64 `json:"speedup,omitempty"`
	CPU       *float64 `json:"cpu_ratio,omitempty"`
	RSS       *float64 `json:"rss_ratio,omitempty"`
	Size      *float64 `json:"size_ratio,omitempty"`
}

// Row is one scenario's result.
type Row struct {
	Scenario     string             `json:"scenario"`
	Group        string             `json:"group"`
	Title        string             `json:"title"`
	Input        string             `json:"input"`
	Threads      int                `json:"threads,omitempty"`
	Preset       int                `json:"preset,omitempty"`
	Filter       string             `json:"filter,omitempty"`
	Direction    string             `json:"direction"`
	Metric       string             `json:"metric"`
	Status       string             `json:"status"`
	Reason       string             `json:"reason,omitempty"`
	NoReference  string             `json:"no_reference,omitempty"`
	PayloadBytes int64              `json:"payload_bytes"`
	Load         Stat               `json:"load_before"`
	Contenders   []ContenderSummary `json:"contenders"`
	Ratios       []Ratio            `json:"ratios"`
}

// Report is report.json.
type Report struct {
	Schema        string                    `json:"schema"`
	SchemaVersion int                       `json:"schema_version"`
	Generated     string                    `json:"generated"`
	Started       string                    `json:"started"`
	Finished      string                    `json:"finished"`
	Host          hostinfo.Host             `json:"host"`
	Toolchain     Toolchain                 `json:"toolchain"`
	Settings      RunSettings               `json:"settings"`
	Orientation   Orientation               `json:"orientation"`
	Rows          []Row                     `json:"rows"`
	RSS           []procmeasure.RSSScenario `json:"rss"`
	Skipped       []SkippedScenario         `json:"skipped"`
	Dropped       []DroppedContender        `json:"dropped"`
	Failures      []string                  `json:"failures"`
	DNF           []string                  `json:"dnf"`
}

// OK reports whether the run had no failures (a reference's DNF is not one).
func (r *Report) OK() bool { return len(r.Failures) == 0 }

func ptr(v float64) *float64 { return &v }

// BuildReport derives the report from raw runs.
func BuildReport(raw *Raw) *Report {
	report := &Report{
		Schema: ReportSchema, SchemaVersion: SchemaVersion,
		Generated: time.Now().UTC().Format(time.RFC3339), Started: raw.Started, Finished: raw.Finished,
		Host: raw.Host, Toolchain: raw.Toolchain, Settings: raw.Settings,
		Orientation: Orientation{Ratio: RatioOrientation, RarparBench: RarparNote},
		Skipped:     raw.Skipped, Dropped: raw.Dropped,
		Failures: []string{}, DNF: []string{},
	}
	if report.Skipped == nil {
		report.Skipped = []SkippedScenario{}
	}
	if report.Dropped == nil {
		report.Dropped = []DroppedContender{}
	}
	// Warmups go in too: they are kept out of the figures, but a contender
	// that fails one never reaches its measured passes, so the warmup is the
	// only record of that failure and must reach the row.
	runs := map[string][]RunRecord{}
	for _, run := range raw.Runs {
		key := run.Scenario + "\x00" + run.Contender
		runs[key] = append(runs[key], run)
	}
	skipped := map[string]bool{}
	for _, s := range raw.Skipped {
		skipped[s.Scenario] = true
	}
	for _, scenario := range raw.Scenarios {
		if skipped[scenario.ID] {
			continue
		}
		row := Row{Scenario: scenario.ID, Group: scenario.Group, Title: scenario.Title, Input: scenario.Input,
			Threads: scenario.Threads, Preset: scenario.Preset, Filter: scenario.Filter, Direction: scenario.Direction,
			Metric: scenario.Metric, NoReference: scenario.NoReference, Status: StatusOK, Ratios: []Ratio{}}
		if d, ok := raw.Fixtures[scenario.Source]; ok {
			row.PayloadBytes = d.Size * int64(max(scenario.Repeat, 1))
		}
		var loads []float64
		var ours *ContenderSummary
		for _, contender := range scenario.Contenders {
			list := runs[scenario.ID+"\x00"+contender.Name]
			if len(list) == 0 {
				continue
			}
			summary := summarize(scenario, contender, list, row.PayloadBytes)
			for _, run := range list {
				if run.LoadBefore >= 0 && !run.Warmup {
					loads = append(loads, run.LoadBefore)
				}
			}
			switch summary.Status {
			case StatusFailed:
				report.Failures = append(report.Failures, fmt.Sprintf("%s %s: %s", scenario.ID, contender.Name, summary.Reason))
				row.Status, row.Reason = StatusFailed, summary.Reason
			case StatusDNF:
				report.DNF = append(report.DNF, fmt.Sprintf("%s %s: %s", scenario.ID, contender.Name, summary.Reason))
			}
			row.Contenders = append(row.Contenders, summary)
		}
		if len(row.Contenders) == 0 {
			continue
		}
		row.Load = stat(loads)
		for i := range row.Contenders {
			if row.Contenders[i].Role == RoleOurs {
				ours = &row.Contenders[i]
			}
		}
		if ours != nil && ours.Status == StatusOK {
			for _, ref := range row.Contenders {
				if ref.Role != RoleReference || ref.Status != StatusOK {
					continue
				}
				ratio := Ratio{Reference: ref.Name}
				if ours.Seconds.Median > 0 {
					ratio.Speedup = ptr(ref.Seconds.Median / ours.Seconds.Median)
				}
				if ours.CPU.Median > 0 {
					ratio.CPU = ptr(ref.CPU.Median / ours.CPU.Median)
				}
				if ref.MaxRSSBytes > 0 && ours.MaxRSSBytes > 0 {
					ratio.RSS = ptr(float64(ref.MaxRSSBytes) / float64(ours.MaxRSSBytes))
				}
				if scenario.Direction == "encode" && ref.BytesOut > 0 && ours.BytesOut > 0 {
					ratio.Size = ptr(float64(ref.BytesOut) / float64(ours.BytesOut))
				}
				row.Ratios = append(row.Ratios, ratio)
			}
			rss := procmeasure.RSSScenario{Scenario: scenario.ID, OursSource: ours.RSSSource}
			rss.OursMedianBytes, rss.OursMinBytes, rss.OursMaxBytes = int64(ours.MaxRSS.Median), int64(ours.MaxRSS.Min), int64(ours.MaxRSS.Max)
			for _, ref := range row.Contenders {
				if ref.Role == RoleReference && ref.Status == StatusOK {
					rss.Reference, rss.ReferenceSource = ref.Name, ref.RSSSource
					rss.ReferenceMedianBytes, rss.ReferenceMinBytes, rss.ReferenceMaxBytes = int64(ref.MaxRSS.Median), int64(ref.MaxRSS.Min), int64(ref.MaxRSS.Max)
					break
				}
			}
			procmeasure.CompleteRSSScenario(&rss)
			report.RSS = append(report.RSS, rss)
		}
		report.Rows = append(report.Rows, row)
	}
	procmeasure.SortRSSScenarios(report.RSS)
	return report
}

func summarize(scenario Scenario, contender Contender, runs []RunRecord, payload int64) ContenderSummary {
	summary := ContenderSummary{Name: contender.Name, Role: contender.Role, Status: StatusOK, Command: runs[0].Command}
	var seconds, wall, cpu, rss []float64
	var sources []string
	var bytesOut []float64
	for _, run := range runs {
		if run.Status != StatusOK {
			summary.Status, summary.Reason = run.Status, run.Reason
			continue
		}
		if run.Warmup {
			continue
		}
		summary.Runs++
		wall = append(wall, run.WallSeconds)
		cpu = append(cpu, run.UserSeconds+run.SysSeconds)
		rss = append(rss, float64(run.MaxRSSBytes))
		sources = append(sources, run.RSSSource)
		seconds = append(seconds, timeOf(scenario, run))
		if run.BytesOut > 0 {
			bytesOut = append(bytesOut, float64(run.BytesOut))
		}
		if run.BytesIn > summary.BytesIn {
			summary.BytesIn = run.BytesIn
		}
		if run.Shot != nil {
			summary.PeakAllocBytes = max(summary.PeakAllocBytes, run.Shot.PeakAllocBytes)
			summary.InputBufferedBytes = max(summary.InputBufferedBytes, run.Shot.InputBufferedBytes)
		}
	}
	if summary.Runs > 0 && summary.Status != StatusOK && summary.Role == RoleReference {
		// A reference that finished some runs and not others is still a DNF:
		// its figures would be the lucky runs only.
		summary.Status = StatusDNF
	}
	summary.Seconds, summary.Wall, summary.CPU, summary.MaxRSS = stat(seconds), stat(wall), stat(cpu), stat(rss)
	summary.MaxRSSBytes = int64(summary.MaxRSS.Median)
	summary.RSSSource = procmeasure.JoinSources(sources)
	summary.BytesOut = int64(stat(bytesOut).Median)
	if summary.Seconds.Median > 0 && payload > 0 {
		summary.MiBPerSecond = float64(payload) / (1 << 20) / summary.Seconds.Median
	}
	return summary
}

// WriteReports writes report.json and report.md beside each other.
func WriteReports(report *Report, jsonPath, mdPath string) error {
	if err := WriteJSON(jsonPath, report); err != nil {
		return err
	}
	return os.WriteFile(mdPath, []byte(RenderMarkdown(report)), 0o644)
}

// ReadReport loads a report.json.
func ReadReport(path string) (*Report, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var report Report
	if err := json.Unmarshal(data, &report); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if report.Schema != ReportSchema || report.SchemaVersion != SchemaVersion {
		return nil, fmt.Errorf("%s: schema %s v%d, want %s v%d", path, report.Schema, report.SchemaVersion, ReportSchema, SchemaVersion)
	}
	return &report, nil
}

func ratioText(v *float64) string {
	if v == nil {
		return "-"
	}
	return fmt.Sprintf("%.3f", *v)
}

// groupTitles orders and names the report's sections.
var groupTitles = []struct{ group, title string }{
	{"decode-lzma1", "Decode: LZMA1 (.lzma)"},
	{"decode-xz", "Decode: .xz, one thread"},
	{"decode-7z", "Decode: .7z single stream"},
	{"decode-7z-mt", "Decode: .7z multi-chunk thread sweep"},
	{"decode-xz-par", "Decode: multi-block .xz thread sweep (XzParallelReader)"},
	{"encode", "Encode: .xz, one thread"},
	{"encode-mt", "Encode: multi-threaded .xz"},
	{"filter", "Filters in an .xz chain"},
	{"filter-raw", "Bare converters (in-process timed region)"},
	{"check", "Integrity checks"},
}

// RenderMarkdown writes report.md.
func RenderMarkdown(report *Report) string {
	var b strings.Builder
	host := report.Host
	tc := report.Toolchain
	fmt.Fprintln(&b, "# lzma-turbo-bench report")
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "- Host: `%s`: %s/%s, %s, %d logical CPUs", host.Label, host.OS, host.Architecture, dash(host.CPU), host.CPUCount)
	if host.MemoryBytes > 0 {
		fmt.Fprintf(&b, ", %.1f GiB", float64(host.MemoryBytes)/(1<<30))
	}
	fmt.Fprintln(&b)
	fmt.Fprintf(&b, "- Kernel: %s\n", dash(host.Kernel))
	fmt.Fprintf(&b, "- ISA tier: `%s` (%s: %s)\n", host.Tier, host.ISASource, strings.Join(host.ISA, " "))
	dirty := ""
	if tc.Crate.Dirty {
		dirty = " (dirty tree)"
	}
	fmt.Fprintf(&b, "- lzma-turbo %s at %s%s; %s; %s; %s\n", tc.Crate.Version, dash(shortCommit(tc.Crate.Commit)), dirty, dash(tc.Rustc), tc.Go, dash(tc.Cargo))
	fmt.Fprintf(&b, "- xz: %s; 7-Zip: %s; 7lzma: %s\n", toolText(tc.XZ), toolText(tc.SevenZip), toolText(tc.SevenLZMA))
	if len(tc.Crates) > 0 {
		var names []string
		for name := range tc.Crates {
			names = append(names, name)
		}
		sort.Strings(names)
		var parts []string
		for _, name := range names {
			parts = append(parts, name+" "+tc.Crates[name])
		}
		fmt.Fprintf(&b, "- Cargo.lock: %s\n", strings.Join(parts, ", "))
	}
	fmt.Fprintf(&b, "- Run: %s to %s, %d repeat(s), %d warmup(s)%s; load average before each run is in report.json\n",
		report.Started, report.Finished, report.Settings.Repeats, report.Settings.Warmups, profileNote(report.Settings))
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, "## How to read the ratios")
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, "- "+RatioOrientation)
	fmt.Fprintln(&b, "- "+RarparNote)
	fmt.Fprintln(&b, "- Times are seconds, median [min–max] over the measured runs; contenders run interleaved, the order reversed on every other repeat. MiB/s is on the uncompressed bytes.")
	fmt.Fprintln(&b)
	renderTierNote(&b, report)
	procmeasure.RenderRSSSummary(&b, report.RSS)

	byGroup := map[string][]Row{}
	for _, row := range report.Rows {
		byGroup[row.Group] = append(byGroup[row.Group], row)
	}
	for _, g := range groupTitles {
		rows := byGroup[g.group]
		if len(rows) == 0 {
			continue
		}
		fmt.Fprintf(&b, "## %s\n\n", g.title)
		if g.group == "filter-raw" && rows[0].NoReference != "" {
			fmt.Fprintf(&b, "lzma-turbo only: %s. Time is the shot's timed region; the input is buffered whole. A pass over the BCJ fixture takes milliseconds, so those rows need repeats before a difference means anything.\n\n", rows[0].NoReference)
		}
		fmt.Fprintln(&b, "| scenario | contender | time s | MiB/s | CPU s | peak RSS MiB | speedup | CPU ratio | RSS ratio | size ratio |")
		fmt.Fprintln(&b, "|---|---|---|---|---|---|---|---|---|---|")
		for _, row := range rows {
			ratios := map[string]Ratio{}
			for _, r := range row.Ratios {
				ratios[r.Reference] = r
			}
			for _, c := range row.Contenders {
				name := c.Name
				if c.Status != StatusOK {
					name += " (" + strings.ToUpper(c.Status) + ")"
				}
				r := ratios[c.Name]
				speed := "-"
				if c.MiBPerSecond > 0 {
					speed = fmt.Sprintf("%.1f", c.MiBPerSecond)
				}
				fmt.Fprintf(&b, "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", row.Scenario, name, seconds(c.Seconds), speed,
					seconds(c.CPU), mebibytes(c.MaxRSS), ratioText(r.Speedup), ratioText(r.CPU), ratioText(r.RSS), ratioText(r.Size))
			}
		}
		fmt.Fprintln(&b)
		if g.group == "encode" || g.group == "encode-mt" || g.group == "filter" {
			renderSizes(&b, rows)
		}
	}
	renderCheckCost(&b, byGroup["check"])
	renderAllocations(&b, report.Rows)

	if len(report.Skipped) > 0 {
		fmt.Fprintln(&b, "## Skipped scenarios")
		fmt.Fprintln(&b)
		for _, s := range report.Skipped {
			fmt.Fprintf(&b, "- `%s`: %s\n", s.Scenario, s.Reason)
		}
		fmt.Fprintln(&b)
	}
	if len(report.Dropped) > 0 {
		fmt.Fprintln(&b, "## References not run")
		fmt.Fprintln(&b)
		reasons := map[string][]string{}
		var order []string
		for _, d := range report.Dropped {
			key := d.Contender + ": " + d.Reason
			if reasons[key] == nil {
				order = append(order, key)
			}
			reasons[key] = append(reasons[key], d.Scenario)
		}
		for _, key := range order {
			fmt.Fprintf(&b, "- %s (%d scenarios)\n", key, len(reasons[key]))
		}
		fmt.Fprintln(&b)
	}
	if len(report.DNF) > 0 {
		fmt.Fprintln(&b, "## Did not finish (references)")
		fmt.Fprintln(&b)
		for _, d := range report.DNF {
			fmt.Fprintf(&b, "- %s\n", d)
		}
		fmt.Fprintln(&b)
	}
	fmt.Fprintln(&b, "## Failures")
	fmt.Fprintln(&b)
	if len(report.Failures) == 0 {
		fmt.Fprintln(&b, "None.")
	}
	for _, f := range report.Failures {
		fmt.Fprintf(&b, "- %s\n", f)
	}
	return b.String()
}

func shortCommit(commit string) string {
	if len(commit) > 12 {
		return commit[:12]
	}
	return commit
}

func toolText(t Tool) string {
	if !t.Found() {
		return "not found"
	}
	return fmt.Sprintf("%s (%s)", dash(t.Version), t.Provenance)
}

func renderTierNote(b *strings.Builder, report *Report) {
	fmt.Fprintln(b, "## ISA tier")
	fmt.Fprintln(b)
	host := report.Host
	fmt.Fprintf(b, "Host tier `%s`. lzma-turbo has no run-time ISA dispatch of its own, so every row on this host ran the same code paths:\n\n", host.Tier)
	if build := report.Toolchain.Build; build != nil {
		loop := "the portable Rust decode loop"
		if build.AsmLoop {
			loop = "the assembly decode loop"
		}
		fmt.Fprintf(b, "- LZMA decode: %s, chosen at compile time for %s-%s (compile-time target features: %s).\n", loop, build.TargetArch, build.TargetOS, dash(strings.Join(build.CompileTargetFeatures, " ")))
		fmt.Fprintf(b, "- CRC-32 / CRC-64: crc-fast, which picked `%s` / `%s` at run time on this host.\n", build.CRC32Tier, build.CRC64Tier)
		fmt.Fprintf(b, "- SHA-256: %s, which dispatches internally.\n", build.SHA256Backend)
	} else {
		fmt.Fprintln(b, "- (lzma-bench --shot info was unavailable; the build facts are not recorded.)")
	}
	fmt.Fprintln(b, "- Match finding, the range coder and the BCJ/delta converters are portable Rust; the BCJ fixtures are this host's own machine code, so a converter for another architecture converts little of it.")
	fmt.Fprintln(b, "- The oracles dispatch as they were built: xz and 7-Zip choose their own CRC and match-finder kernels.")
	fmt.Fprintln(b)
}

func renderSizes(b *strings.Builder, rows []Row) {
	has := false
	for _, row := range rows {
		if row.Direction == "encode" {
			has = true
		}
	}
	if !has {
		return
	}
	fmt.Fprintln(b, "Output sizes (bytes):")
	fmt.Fprintln(b)
	fmt.Fprintln(b, "| scenario | lzma-turbo | reference | size ratio |")
	fmt.Fprintln(b, "|---|---|---|---|")
	for _, row := range rows {
		if row.Direction != "encode" {
			continue
		}
		var ours, ref ContenderSummary
		for _, c := range row.Contenders {
			if c.Role == RoleOurs {
				ours = c
			} else if ref.Name == "" && c.Status == StatusOK {
				ref = c
			}
		}
		size := "-"
		for _, r := range row.Ratios {
			if r.Reference == ref.Name {
				size = ratioText(r.Size)
			}
		}
		fmt.Fprintf(b, "| %s | %d | %s %d | %s |\n", row.Scenario, ours.BytesOut, dash(ref.Name), ref.BytesOut, size)
	}
	fmt.Fprintln(b)
}

// renderCheckCost subtracts the check=none decode from each check's, for
// lzma-turbo and xz alike: what verifying the stream costs on this host.
func renderCheckCost(b *strings.Builder, rows []Row) {
	if len(rows) == 0 {
		return
	}
	base := map[string]float64{}
	for _, row := range rows {
		if row.Scenario == "check/none" {
			for _, c := range row.Contenders {
				if c.Status == StatusOK {
					base[c.Name] = c.Seconds.Median
				}
			}
		}
	}
	if len(base) == 0 {
		return
	}
	fmt.Fprintln(b, "### Check cost")
	fmt.Fprintln(b)
	fmt.Fprintln(b, "Median decode time minus the `check=none` decode's, seconds, and the check's own throughput on the 256 MiB payload. Small differences sit inside run-to-run noise.")
	fmt.Fprintln(b)
	fmt.Fprintln(b, "| check | contender | extra s | check MiB/s |")
	fmt.Fprintln(b, "|---|---|---|---|")
	for _, row := range rows {
		if row.Scenario == "check/none" {
			continue
		}
		for _, c := range row.Contenders {
			zero, ok := base[c.Name]
			if !ok || c.Status != StatusOK {
				continue
			}
			extra := c.Seconds.Median - zero
			rate := "-"
			if extra > 0 && row.PayloadBytes > 0 {
				rate = fmt.Sprintf("%.0f", float64(row.PayloadBytes)/(1<<20)/extra)
			}
			fmt.Fprintf(b, "| %s | %s | %.3f | %s |\n", strings.TrimPrefix(row.Scenario, "check/"), c.Name, extra, rate)
		}
	}
	fmt.Fprintln(b)
}

func renderAllocations(b *strings.Builder, rows []Row) {
	fmt.Fprintln(b, "## In-process peak allocation")
	fmt.Fprintln(b)
	fmt.Fprintln(b, "The Rust heap high-water mark each shot saw in its timed region, beside the process's peak RSS: the gap is the binary, the stack, buffered input and the allocator's slack. liblzma's C allocations are not on the Rust heap, so its figure is near zero and only its RSS speaks for it.")
	fmt.Fprintln(b)
	fmt.Fprintln(b, "| scenario | contender | peak alloc MiB | peak RSS MiB | input buffered MiB |")
	fmt.Fprintln(b, "|---|---|---|---|---|")
	for _, row := range rows {
		for _, c := range row.Contenders {
			if (c.PeakAllocBytes == 0 && c.InputBufferedBytes == 0) || c.Status != StatusOK {
				continue
			}
			buffered := "-"
			if c.InputBufferedBytes > 0 {
				buffered = procmeasure.MiB(c.InputBufferedBytes)
			}
			fmt.Fprintf(b, "| %s | %s | %s | %s | %s |\n", row.Scenario, c.Name, procmeasure.MiB(c.PeakAllocBytes), procmeasure.MiB(c.MaxRSSBytes), buffered)
		}
	}
	fmt.Fprintln(b)
}

// profileNote names the matrix profile in the report header; runs from
// before profiles existed carry only the quick flag.
func profileNote(s RunSettings) string {
	switch {
	case s.Profile != "":
		return ", " + s.Profile + " profile"
	case s.Quick:
		return ", quick matrix"
	}
	return ""
}
