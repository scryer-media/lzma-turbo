package harness

import (
	"fmt"
	"sort"
	"strings"

	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/procmeasure"
)

// MergeMarkdown renders one cross-architecture report from several hosts'
// report.json files: the hosts, then lzma-turbo's speedup over each
// scenario's primary reference, its own time, and its peak RSS, one column
// per host. A scenario's ID is the same on every host ("tall" is every
// logical CPU wherever it ran), so the rows line up.
func MergeMarkdown(reports []*Report) string {
	var b strings.Builder
	fmt.Fprintln(&b, "# lzma-turbo-bench: cross-host report")
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, "- "+RatioOrientation)
	fmt.Fprintln(&b, "- "+RarparNote)
	fmt.Fprintln(&b, "- Each cell is the median over that host's measured runs; `-` is a scenario the host skipped or could not compare. The primary reference is the first that finished: 7zz for LZMA1 and .7z, xz for .xz, encode, filters and checks.")
	fmt.Fprintln(&b)

	fmt.Fprintln(&b, "## Hosts")
	fmt.Fprintln(&b)
	fmt.Fprintln(&b, "| host | os/arch | CPU | CPUs | tier | CRC-32 tier | lzma-turbo | xz | 7-Zip | runs |")
	fmt.Fprintln(&b, "|---|---|---|---|---|---|---|---|---|---|")
	for _, r := range reports {
		crc := "-"
		if r.Toolchain.Build != nil {
			crc = r.Toolchain.Build.CRC32Tier
		}
		dirty := ""
		if r.Toolchain.Crate.Dirty {
			dirty = "+dirty"
		}
		fmt.Fprintf(&b, "| %s | %s/%s | %s | %d | %s | %s | %s %s%s | %s | %s | %s, %d repeat(s) |\n", r.Host.Label, r.Host.OS, r.Host.Architecture,
			dash(r.Host.CPU), r.Host.CPUCount, r.Host.Tier, crc, r.Toolchain.Crate.Version, shortCommit(r.Toolchain.Crate.Commit), dirty,
			dash(r.Toolchain.XZ.Version), dash(r.Toolchain.SevenZip.Version), r.Started, r.Settings.Repeats)
	}
	fmt.Fprintln(&b)

	commits := map[string]bool{}
	for _, r := range reports {
		commits[r.Toolchain.Crate.Commit] = true
	}
	if len(commits) > 1 {
		fmt.Fprintln(&b, "Warning: the hosts measured different lzma-turbo commits; a column difference may be the code, not the machine.")
		fmt.Fprintln(&b)
	}

	var ids []string
	seen := map[string]bool{}
	index := make([]map[string]Row, len(reports))
	for i, r := range reports {
		index[i] = map[string]Row{}
		for _, row := range r.Rows {
			index[i][row.Scenario] = row
			if !seen[row.Scenario] {
				seen[row.Scenario] = true
				ids = append(ids, row.Scenario)
			}
		}
	}
	order := map[string]int{}
	for i, g := range groupTitles {
		order[g.group] = i
	}
	group := map[string]string{}
	for i := range reports {
		for id, row := range index[i] {
			group[id] = row.Group
		}
	}
	sort.SliceStable(ids, func(a, c int) bool { return order[group[ids[a]]] < order[group[ids[c]]] })

	header := func(title, note string) {
		fmt.Fprintf(&b, "## %s\n\n%s\n\n| scenario |", title, note)
		for _, r := range reports {
			fmt.Fprintf(&b, " %s |", r.Host.Label)
		}
		fmt.Fprint(&b, "\n|---|")
		for range reports {
			fmt.Fprint(&b, "---|")
		}
		fmt.Fprintln(&b)
	}
	table := func(title, note string, value func(Row) string) {
		header(title, note)
		for _, id := range ids {
			fmt.Fprintf(&b, "| %s |", id)
			for i := range reports {
				row, ok := index[i][id]
				text := "-"
				if ok {
					text = value(row)
				}
				fmt.Fprintf(&b, " %s |", text)
			}
			fmt.Fprintln(&b)
		}
		fmt.Fprintln(&b)
	}
	ours := func(row Row) *ContenderSummary {
		for i := range row.Contenders {
			if row.Contenders[i].Role == RoleOurs && row.Contenders[i].Status == StatusOK {
				return &row.Contenders[i]
			}
		}
		return nil
	}
	table("Speedup over the primary reference", "ratio = reference / lzma-turbo time, >1 = lzma-turbo better (faster).", func(row Row) string {
		if len(row.Ratios) == 0 || row.Ratios[0].Speedup == nil {
			return "-"
		}
		return fmt.Sprintf("%.3f (%s)", *row.Ratios[0].Speedup, row.Ratios[0].Reference)
	})
	table("lzma-turbo time", "Seconds, median [min–max].", func(row Row) string {
		if c := ours(row); c != nil {
			return seconds(c.Seconds)
		}
		return "-"
	})
	table("lzma-turbo peak RSS", "MiB, median, and the RSS ratio against the primary reference (ratio = reference / lzma-turbo, >1 = lzma-turbo better: a lower peak).", func(row Row) string {
		c := ours(row)
		if c == nil {
			return "-"
		}
		text := procmeasure.MiB(c.MaxRSSBytes)
		if len(row.Ratios) > 0 && row.Ratios[0].RSS != nil {
			text += fmt.Sprintf(" (%.3f)", *row.Ratios[0].RSS)
		}
		return text
	})

	var problems []string
	for _, r := range reports {
		for _, f := range r.Failures {
			problems = append(problems, r.Host.Label+": "+f)
		}
	}
	fmt.Fprintln(&b, "## Failures")
	fmt.Fprintln(&b)
	if len(problems) == 0 {
		fmt.Fprintln(&b, "None.")
	}
	for _, p := range problems {
		fmt.Fprintf(&b, "- %s\n", p)
	}
	return b.String()
}
