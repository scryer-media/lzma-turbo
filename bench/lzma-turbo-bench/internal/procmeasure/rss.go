package procmeasure

import (
	"fmt"
	"math"
	"sort"
	"strings"
)

// Peak resident set sources. Every one of them reads the measured tool's own
// process: the harness launches each tool as its direct child.
const (
	// RSSSourceRusage is the direct child's wait4 rusage ru_maxrss (bytes on
	// macOS, KiB normalised to bytes on Linux), the figure /usr/bin/time
	// prints as the maximum resident set size.
	RSSSourceRusage = "rusage"
	// RSSSourcePeakWorkingSet is Windows PeakWorkingSetSize from
	// K32GetProcessMemoryInfo on a handle held across the child's exit.
	RSSSourcePeakWorkingSet = "peak-working-set"
)

// FailureMissingRSS classifies a run whose process exited but whose peak RSS
// the harness did not capture. It is a harness failure, never a silent zero.
const FailureMissingRSS = "harness-missing-rss"

// DescribeRSSSource explains a source tag for reports and error messages.
func DescribeRSSSource(source string) string {
	switch source {
	case RSSSourceRusage:
		return "rusage ru_maxrss of the tool as the harness's direct child"
	case RSSSourcePeakWorkingSet:
		return "Windows PeakWorkingSetSize of the tool's process, read through a handle held across its exit"
	case "":
		return "no peak RSS recorded"
	default:
		return source
	}
}

// MultiProcessNote is the caveat every RSS report carries.
const MultiProcessNote = "Peak RSS is one process's high-water mark. On POSIX the figure also covers descendants the tool reaped (the largest single peak, not their sum); on Windows it is the tool's process alone. lzma-turbo, xz, 7zz and 7lzma each run as one process here, so the figure is the tool's whole footprint. lzma-rust2 and liblzma rows run inside the Rust bench tool, so theirs include that tool's own small image."

// RSSScenario compares one scenario's peak resident set: lzma-turbo's peak
// against the scenario's primary reference, both medians [min, max] over the
// measured runs. The field names follow rarpar-bench's, with "ours" where
// that harness says "rarpar".
type RSSScenario struct {
	Scenario string `json:"scenario"`
	// Reference names the reference row the ratio is taken against.
	Reference            string `json:"reference,omitempty"`
	OursMedianBytes      int64  `json:"ours_median_bytes"`
	OursMinBytes         int64  `json:"ours_min_bytes"`
	OursMaxBytes         int64  `json:"ours_max_bytes"`
	ReferenceMedianBytes int64  `json:"reference_median_bytes,omitempty"`
	ReferenceMinBytes    int64  `json:"reference_min_bytes,omitempty"`
	ReferenceMaxBytes    int64  `json:"reference_max_bytes,omitempty"`
	// Ratio is reference/ours medians: above 1 lzma-turbo peaked lower. It
	// is absent when the scenario has no reference figure.
	Ratio           *float64 `json:"ratio,omitempty"`
	OursSource      string   `json:"ours_rss_source"`
	ReferenceSource string   `json:"reference_rss_source,omitempty"`
	// Note says why there is no ratio, or that the two sides were not
	// measured the same way.
	Note string `json:"note,omitempty"`
}

// PeakStats is the median, min and max of a set of peaks, in bytes.
func PeakStats(values []int64) (median, low, high int64) {
	if len(values) == 0 {
		return 0, 0, 0
	}
	sorted := append([]int64(nil), values...)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i] < sorted[j] })
	middle := len(sorted) / 2
	median = sorted[middle]
	if len(sorted)%2 == 0 {
		median = int64(math.Round((float64(sorted[middle-1]) + float64(sorted[middle])) / 2))
	}
	return median, sorted[0], sorted[len(sorted)-1]
}

// JoinSources renders the distinct sources of a set of runs, sorted.
func JoinSources(sources []string) string {
	seen := map[string]bool{}
	var distinct []string
	for _, source := range sources {
		if source != "" && !seen[source] {
			seen[source] = true
			distinct = append(distinct, source)
		}
	}
	sort.Strings(distinct)
	return strings.Join(distinct, "+")
}

// CompleteRSSScenario fills the ratio and the comparability note once both
// sides' figures and sources are set.
func CompleteRSSScenario(scenario *RSSScenario) {
	if scenario.ReferenceMedianBytes > 0 && scenario.OursMedianBytes > 0 {
		value := float64(scenario.ReferenceMedianBytes) / float64(scenario.OursMedianBytes)
		scenario.Ratio = &value
	}
	if scenario.Ratio != nil && scenario.OursSource != scenario.ReferenceSource {
		scenario.Note = fmt.Sprintf("measured differently: ours %s, reference %s", scenario.OursSource, scenario.ReferenceSource)
	}
}

// SortRSSScenarios orders the summary worst ratio first, so an RSS regression
// is at the top. The ratio is reference/ours, so worst is lowest; scenarios
// without a ratio follow, by name.
func SortRSSScenarios(scenarios []RSSScenario) {
	sort.SliceStable(scenarios, func(i, j int) bool {
		left, right := scenarios[i], scenarios[j]
		if (left.Ratio == nil) != (right.Ratio == nil) {
			return left.Ratio != nil
		}
		if left.Ratio != nil && *left.Ratio != *right.Ratio {
			return *left.Ratio < *right.Ratio
		}
		return left.Scenario < right.Scenario
	})
}

// MiB renders a byte count in MiB.
func MiB(bytes int64) string {
	return fmt.Sprintf("%.1f", float64(bytes)/(1<<20))
}

// MiBRange renders median [min–max] in MiB, or "-" for no figure.
func MiBRange(median, low, high int64) string {
	if median <= 0 {
		return "-"
	}
	if low == high {
		return MiB(median)
	}
	return fmt.Sprintf("%s [%s–%s]", MiB(median), MiB(low), MiB(high))
}

// RenderRSSSummary writes the "Peak RSS per scenario" Markdown section.
func RenderRSSSummary(b *strings.Builder, scenarios []RSSScenario) {
	fmt.Fprintln(b, "## Peak RSS per scenario")
	fmt.Fprintln(b)
	if len(scenarios) == 0 {
		fmt.Fprintln(b, "No scenario has a measured lzma-turbo peak.")
		fmt.Fprintln(b)
		return
	}
	fmt.Fprintln(b, "Peak resident set, MiB, median [min–max] over the measured runs, against each scenario's primary reference. RSS ratio = reference / lzma-turbo medians, >1 = lzma-turbo better (a lower peak). Sorted worst (lowest) ratio first.")
	fmt.Fprintln(b, MultiProcessNote)
	fmt.Fprintln(b)
	fmt.Fprintln(b, "| scenario | reference | lzma-turbo MiB | reference MiB | RSS ratio | source | note |")
	fmt.Fprintln(b, "|---|---|---|---|---|---|---|")
	for _, scenario := range scenarios {
		ratio := "-"
		if scenario.Ratio != nil {
			ratio = fmt.Sprintf("%.3f", *scenario.Ratio)
		}
		source := scenario.OursSource
		if scenario.ReferenceSource != "" && scenario.ReferenceSource != scenario.OursSource {
			source = "ours " + scenario.OursSource + ", reference " + scenario.ReferenceSource
		}
		fmt.Fprintf(b, "| %s | %s | %s | %s | %s | %s | %s |\n", scenario.Scenario, dash(scenario.Reference),
			MiBRange(scenario.OursMedianBytes, scenario.OursMinBytes, scenario.OursMaxBytes),
			MiBRange(scenario.ReferenceMedianBytes, scenario.ReferenceMinBytes, scenario.ReferenceMaxBytes),
			ratio, dash(source), dash(scenario.Note))
	}
	fmt.Fprintln(b)
	var tags []string
	for _, scenario := range scenarios {
		tags = append(tags, strings.Split(scenario.OursSource, "+")...)
		tags = append(tags, strings.Split(scenario.ReferenceSource, "+")...)
	}
	for _, tag := range strings.Split(JoinSources(tags), "+") {
		if tag != "" {
			fmt.Fprintf(b, "- `%s`: %s\n", tag, DescribeRSSSource(tag))
		}
	}
	fmt.Fprintln(b)
}

func dash(value string) string {
	if value == "" {
		return "-"
	}
	return value
}
