package harness

import (
	"context"
	"encoding/json"
	"fmt"
	"hash/crc32"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"testing"

	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/hostinfo"
	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/procmeasure"
)

// fakeShotEnv makes the test binary stand in for `lzma-bench --shot`: it
// prints a shot line, with the input file's CRC under --verify as a
// decoder's is, and none otherwise. Set to "bad", the CRC is wrong.
const fakeShotEnv = "LZMA_TURBO_BENCH_TEST_FAKE_SHOT"

func TestMain(m *testing.M) {
	if os.Getenv(fakeShotEnv) != "" {
		input := os.Args[len(os.Args)-1]
		data, err := os.ReadFile(input)
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		line := ShotLine{Lane: "xz", Direction: "decode", BytesIn: int64(len(data)), BytesOut: int64(len(data)),
			InprocSeconds: 0.001, PeakAllocBytes: 4096}
		for _, arg := range os.Args[1:] {
			if arg == "--verify" {
				crc := crc32.ChecksumIEEE(data)
				if os.Getenv(fakeShotEnv) == "bad" {
					crc++
				}
				line.CRC32 = fmt.Sprintf("%08x", crc)
			}
		}
		out, _ := json.Marshal(line)
		fmt.Println(string(out))
		os.Exit(0)
	}
	os.Exit(m.Run())
}

func repoFile(t *testing.T, rel string) string {
	t.Helper()
	path := filepath.Join("..", "..", "..", "..", rel)
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("%s: %v", rel, err)
	}
	return path
}

func TestTheFixtureTableMatchesXtaskAndTheExtras(t *testing.T) {
	file, err := os.Open(repoFile(t, "bench/fixtures/README.md"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	table, err := ParseFixtureTable(file)
	if err != nil {
		t.Fatal(err)
	}
	var documented, documentedExtras []string
	for _, f := range table {
		if f.Extra {
			documentedExtras = append(documentedExtras, f.Name)
		} else {
			documented = append(documented, f.Name)
		}
		if (f.Name == "payload.bin" || f.Name == "p256.bin") && f.SHA256 == "" {
			t.Errorf("%s is seeded and must carry its SHA-256", f.Name)
		}
	}

	source, err := os.ReadFile(repoFile(t, "xtask/src/fixtures.rs"))
	if err != nil {
		t.Fatal(err)
	}
	var generated []string
	for _, m := range regexp.MustCompile(`make\(\s*"([^"]+)"`).FindAllStringSubmatch(string(source), -1) {
		generated = append(generated, m[1])
	}
	for _, m := range regexp.MustCompile(`\("([a-z0-9.]+\.7z)", "-mmt=`).FindAllStringSubmatch(string(source), -1) {
		generated = append(generated, m[1])
	}
	assertSameNames(t, "xtask fixtures", documented, generated)

	var extras []string
	for _, e := range extraFixtures() {
		extras = append(extras, e.Name)
	}
	assertSameNames(t, "harness extras", documentedExtras, extras)
}

func assertSameNames(t *testing.T, what string, documented, actual []string) {
	t.Helper()
	sort.Strings(documented)
	sort.Strings(actual)
	if strings.Join(documented, " ") != strings.Join(actual, " ") {
		t.Errorf("%s: README lists %v, code makes %v", what, documented, actual)
	}
}

func TestEveryMatrixInputIsADocumentedFixture(t *testing.T) {
	file, err := os.Open(repoFile(t, "bench/fixtures/README.md"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	table, err := ParseFixtureTable(file)
	if err != nil {
		t.Fatal(err)
	}
	known := map[string]bool{}
	for _, f := range table {
		known[f.Name] = true
	}
	for _, s := range BuildMatrix(DefaultMatrixOptions(false)) {
		for _, name := range []string{s.Input, s.Source} {
			if name != "" && !known[name] {
				t.Errorf("%s reads %s, which bench/fixtures/README.md does not list", s.ID, name)
			}
		}
	}
}

func TestSizeRules(t *testing.T) {
	for _, c := range []struct {
		rule string
		size int64
		ok   bool
	}{
		{"268435456", 268435456, true},
		{"268435456", 268435455, false},
		{"~224 MiB", 235179900, true},
		{"~224 MiB", 100 << 20, false},
		{"up to 64 MiB", 64 << 20, true},
		{"up to 64 MiB", 65 << 20, false},
		{"varies", 1, true},
		{"varies", 0, false},
	} {
		rule, err := parseSize(c.rule)
		if err != nil {
			t.Fatal(err)
		}
		if got := rule.check(c.size) == ""; got != c.ok {
			t.Errorf("%q on %d: ok=%v, want %v", c.rule, c.size, got, c.ok)
		}
	}
	if _, err := parseSize("about a gigabyte"); err == nil {
		t.Error("an unparseable size was accepted")
	}
}

func TestStatAndOrder(t *testing.T) {
	s := stat([]float64{3, 1, 2, 10})
	if s.Median != 2.5 || s.Min != 1 || s.Max != 10 || s.N != 4 {
		t.Errorf("stat = %+v", s)
	}
	if got := fmt.Sprint(orderFor(3, 0), orderFor(3, 1)); got != "[0 1 2] [2 1 0]" {
		t.Errorf("orderFor = %s", got)
	}
}

func TestTheMatrix(t *testing.T) {
	quick := DefaultMatrixOptions(true)
	quick.CPUs = 4
	full := DefaultMatrixOptions(false)
	full.CPUs = 4
	fleetDefaults, err := ProfileOptions(ProfileFleet)
	if err != nil {
		t.Fatal(err)
	}
	fleet := fleetDefaults.Matrix
	fleet.CPUs = 16
	for name, options := range map[string]MatrixOptions{"quick": quick, "full": full, "fleet": fleet} {
		ids := map[string]bool{}
		for _, s := range BuildMatrix(options) {
			if ids[s.ID] {
				t.Errorf("%s: duplicate scenario %s", name, s.ID)
			}
			ids[s.ID] = true
			ours := 0
			for _, c := range s.Contenders {
				if c.Role == RoleOurs {
					ours++
				}
			}
			if ours != 1 {
				t.Errorf("%s: %s has %d lzma-turbo contenders", name, s.ID, ours)
			}
			if name == "quick" && s.Size != "p256" {
				t.Errorf("quick selected %s", s.ID)
			}
			if s.Group == "encode-mt" && s.Threads == 1 {
				t.Errorf("%s: an MT encode row at one thread", s.ID)
			}
		}
	}
	var sixteen, eight *Scenario
	matrix := BuildMatrix(full)
	for i := range matrix {
		switch matrix[i].ID {
		case "decode/7z-mt/p256/t16":
			sixteen = &matrix[i]
		case "decode/7z-mt/p256/t8":
			eight = &matrix[i]
		}
	}
	if sixteen == nil || sixteen.Skip == "" || eight == nil || eight.Skip == "" {
		t.Error("thread counts above a 4-CPU host's are not skipped with a reason")
	}
	only := full
	only.Only = []string{"encode/st/*"}
	for _, s := range BuildMatrix(only) {
		if !strings.HasPrefix(s.ID, "encode/st/") {
			t.Errorf("--only encode/st/* selected %s", s.ID)
		}
	}
}

func TestFleetProfile(t *testing.T) {
	quickDefaults, _ := ProfileOptions(ProfileQuick)
	quick := quickDefaults.Matrix
	quick.CPUs = 16
	fleetDefaults, err := ProfileOptions(ProfileFleet)
	if err != nil {
		t.Fatal(err)
	}
	if fleetDefaults.Repeats != 3 || fleetDefaults.Warmups != 1 {
		t.Errorf("fleet runs %d repeats + %d warmups, want 3 + 1", fleetDefaults.Repeats, fleetDefaults.Warmups)
	}
	fleet := fleetDefaults.Matrix
	fleet.CPUs = 16
	ids := map[string]bool{}
	var big []string
	for _, s := range BuildMatrix(fleet) {
		ids[s.ID] = true
		if s.Size == "1g" {
			big = append(big, s.ID)
		}
	}
	// Every quick row is a fleet row.
	for _, s := range BuildMatrix(quick) {
		if !ids[s.ID] {
			t.Errorf("fleet drops quick's %s", s.ID)
		}
	}
	if fmt.Sprint(big) != "[decode/xz-par/1g.t8/tall]" {
		t.Errorf("fleet 1 GiB rows = %v, want the one sanity row", big)
	}
	for _, id := range []string{"encode/st/p1", "encode/st/p5", "encode/st/p9",
		"encode/mt/p5/t2", "encode/mt/p5/t4", "encode/mt/p5/t8", "encode/mt/p5/tall"} {
		if !ids[id] {
			t.Errorf("fleet lacks %s", id)
		}
	}
	for id := range ids {
		if strings.HasPrefix(id, "encode/mt/p6") || id == "encode/st/p3" || id == "encode/st/p6" ||
			id == "decode/7z-mt/p256/t4" || id == "decode/xz-par/p256.t8/t8" {
			t.Errorf("fleet selected %s", id)
		}
	}
	if _, err := ProfileOptions("nightly"); err == nil {
		t.Error("an unknown profile is accepted")
	}
}

func syntheticRaw(label string, oursSeconds, refSeconds float64, oursRSS, refRSS int64) *Raw {
	scenario := Scenario{ID: "decode/xz/p256", Group: "decode-xz", Title: "t", Input: "p256.bin.xz", Source: "p256.bin",
		Direction: "decode", Threads: 1, Metric: "wall",
		Contenders: []Contender{{Name: "lzma-turbo", Role: RoleOurs, Kind: KindShot}, {Name: "xz", Role: RoleReference, Kind: KindXZ}}}
	raw := &Raw{Schema: RawSchema, SchemaVersion: SchemaVersion, Host: hostinfo.Host{Label: label, Tier: "neon"},
		Scenarios: []Scenario{scenario}, Fixtures: map[string]Digest{"p256.bin": {Name: "p256.bin", Size: 256 << 20}},
		Settings: RunSettings{Repeats: 3}}
	for repeat, jitter := range []float64{0, 0.1, -0.1} {
		raw.Runs = append(raw.Runs,
			RunRecord{Scenario: scenario.ID, Contender: "lzma-turbo", Role: RoleOurs, Repeat: repeat, Status: StatusOK,
				Measurement: procmeasure.Measurement{WallSeconds: oursSeconds + jitter, UserSeconds: oursSeconds, MaxRSSBytes: oursRSS, RSSSource: procmeasure.RSSSourceRusage},
				Shot:        &ShotLine{PeakAllocBytes: 1 << 20}},
			RunRecord{Scenario: scenario.ID, Contender: "xz", Role: RoleReference, Repeat: repeat, Status: StatusOK,
				Measurement: procmeasure.Measurement{WallSeconds: refSeconds + jitter, UserSeconds: refSeconds, MaxRSSBytes: refRSS, RSSSource: procmeasure.RSSSourceRusage}})
	}
	return raw
}

func TestTheReportOrientsEveryRatio(t *testing.T) {
	report := BuildReport(syntheticRaw("host-a", 1, 2, 30<<20, 10<<20))
	if len(report.Rows) != 1 || len(report.Rows[0].Ratios) != 1 {
		t.Fatalf("rows = %+v", report.Rows)
	}
	ratio := report.Rows[0].Ratios[0]
	if ratio.Speedup == nil || *ratio.Speedup != 2 {
		t.Errorf("speedup = %v, want reference/ours = 2", ratio.Speedup)
	}
	if ratio.RSS == nil || *ratio.RSS != 1.0/3 {
		t.Errorf("RSS ratio = %v, want reference/ours = 0.333", ratio.RSS)
	}
	if got := report.Rows[0].Contenders[0].MiBPerSecond; got != 256 {
		t.Errorf("MiB/s = %v, want 256", got)
	}
	if len(report.RSS) != 1 || report.RSS[0].Ratio == nil {
		t.Errorf("RSS summary = %+v", report.RSS)
	}
	md := RenderMarkdown(report)
	for _, want := range []string{RatioOrientation, RarparNote, "## Peak RSS per scenario", "## ISA tier", "| decode/xz/p256 | xz | 2.000 [1.900–2.100] |"} {
		if !strings.Contains(md, want) {
			t.Errorf("report.md lacks %q", want)
		}
	}

	merged := MergeMarkdown([]*Report{report, BuildReport(syntheticRaw("host-b", 1, 1.5, 10<<20, 10<<20))})
	for _, want := range []string{"| host-a |", "| host-b |", "2.000 (xz)", "1.500 (xz)", "30.0 (0.333)"} {
		if !strings.Contains(merged, want) {
			t.Errorf("merged report lacks %q:\n%s", want, merged)
		}
	}
}

func TestAMissingPeakIsAFailure(t *testing.T) {
	raw := syntheticRaw("host-a", 1, 2, 30<<20, 10<<20)
	raw.Runs[0].Status, raw.Runs[0].Failure, raw.Runs[0].Reason = StatusFailed, procmeasure.FailureMissingRSS, "no peak"
	if report := BuildReport(raw); report.OK() {
		t.Error("a run without a peak RSS did not fail the report")
	}
}

func TestExecuteMeasuresEachContenderAsItsOwnProcess(t *testing.T) {
	dir := t.TempDir()
	payload := []byte(strings.Repeat("lzma-turbo-bench fixture ", 4096))
	if err := os.WriteFile(filepath.Join(dir, "sample.bin"), payload, 0o644); err != nil {
		t.Fatal(err)
	}
	t.Setenv(fakeShotEnv, "1")
	scenario := Scenario{ID: "decode/sample", Group: "decode-xz", Input: "sample.bin", Source: "sample.bin", Direction: "decode", Metric: "wall",
		Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz"), shot("lzma-rust2", RoleReference, "--shot", "rust2-mt")}}
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	raw, err := Execute(context.Background(), RunOptions{
		Paths:     Paths{Fixtures: dir, LzmaBench: self},
		Toolchain: Toolchain{LzmaBench: Tool{Name: "lzma-bench", Path: self}},
		Matrix:    []Scenario{scenario},
		Settings:  RunSettings{Repeats: 2},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(raw.Runs) != 6 {
		t.Fatalf("%d runs, want 2 contenders x (1 verify + 2 repeats)", len(raw.Runs))
	}
	for i, run := range raw.Runs {
		if run.Status != StatusOK {
			t.Errorf("%s %s: %s %s", run.Contender, run.Status, run.Failure, run.Reason)
		}
		if run.MaxRSSBytes <= 0 || run.RSSSource == "" {
			t.Errorf("%s: no peak RSS recorded", run.Contender)
		}
		// The verify runs come first and carry the checked CRC; the timed
		// runs only count, so they carry none.
		if verify := i < 2; run.Verify != verify || strings.Contains(run.Command, "--verify") != verify {
			t.Errorf("run %d (%s): verify %v, command %q", i, run.Contender, run.Verify, run.Command)
		} else if verify && (run.CRC32 == "" || run.CRC32 != run.ExpectedCRC32) {
			t.Errorf("%s: verified CRC %q, want %q", run.Contender, run.CRC32, run.ExpectedCRC32)
		} else if !verify && run.CRC32 != "" {
			t.Errorf("%s: a timed run hashed its output (CRC %s)", run.Contender, run.CRC32)
		}
	}
	if raw.Runs[2].Contender != "lzma-turbo" || raw.Runs[4].Contender != "lzma-rust2" {
		t.Errorf("the second repeat did not reverse the order: %s then %s", raw.Runs[4].Contender, raw.Runs[5].Contender)
	}
	report := BuildReport(raw)
	if !report.OK() || len(report.Rows) != 1 || len(report.Rows[0].Ratios) != 1 || report.Rows[0].Contenders[0].Runs != 2 {
		t.Errorf("report = %+v", report)
	}

	// A wrong CRC in the verify run fails the scenario before any timing.
	t.Setenv(fakeShotEnv, "bad")
	raw, err = Execute(context.Background(), RunOptions{
		Paths:     Paths{Fixtures: dir, LzmaBench: self},
		Toolchain: Toolchain{LzmaBench: Tool{Name: "lzma-bench", Path: self}},
		Matrix:    []Scenario{scenario},
		Settings:  RunSettings{Repeats: 2},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(raw.Runs) != 2 || raw.Runs[0].Failure != "crc-mismatch" {
		t.Fatalf("runs = %+v, want the two verify runs, ours a CRC mismatch", raw.Runs)
	}
	if BuildReport(raw).OK() {
		t.Error("a failed verify run did not fail the report")
	}
}

func TestAMissingFixtureSkipsTheScenario(t *testing.T) {
	scenario := Scenario{ID: "decode/missing", Input: "absent.xz", Source: "absent.bin", Direction: "decode",
		Contenders: []Contender{shot("lzma-turbo", RoleOurs, "--shot", "xz")}}
	raw, err := Execute(context.Background(), RunOptions{
		Paths:     Paths{Fixtures: t.TempDir(), LzmaBench: "lzma-bench"},
		Toolchain: Toolchain{LzmaBench: Tool{Name: "lzma-bench", Path: "lzma-bench"}},
		Matrix:    []Scenario{scenario}, Settings: RunSettings{Repeats: 1},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(raw.Skipped) != 1 || !strings.Contains(raw.Skipped[0].Reason, "absent.xz") || len(raw.Runs) != 0 {
		t.Errorf("skipped = %+v, runs = %d", raw.Skipped, len(raw.Runs))
	}
}

// A contender that fails its warmup never reaches a measured pass, so the
// warmup's failure is the only one there is: it must fail the report rather
// than drop the row.
func TestAWarmupFailureFailsTheReport(t *testing.T) {
	raw := syntheticRaw("host-a", 1, 2, 30<<20, 10<<20)
	warmup := RunRecord{Scenario: "decode/xz/p256", Contender: "lzma-turbo", Role: RoleOurs, Repeat: -1, Warmup: true,
		Status: StatusFailed, Failure: "crc-mismatch", Reason: "output CRC-32 00000000, want 12345678"}
	var kept []RunRecord
	for _, run := range raw.Runs {
		if run.Contender != "lzma-turbo" {
			kept = append(kept, run)
		}
	}
	raw.Runs = append([]RunRecord{warmup}, kept...)
	report := BuildReport(raw)
	if report.OK() {
		t.Fatal("a warmup failure did not fail the report")
	}
	if len(report.Rows) != 1 || report.Rows[0].Status != StatusFailed {
		t.Fatalf("rows = %+v", report.Rows)
	}
	if len(report.Rows[0].Ratios) != 0 {
		t.Errorf("a failed contender has ratios: %+v", report.Rows[0].Ratios)
	}

	// A passing warmup still counts for nothing in the figures.
	raw = syntheticRaw("host-a", 1, 2, 30<<20, 10<<20)
	fast := raw.Runs[0]
	fast.Warmup, fast.Repeat, fast.WallSeconds = true, -1, 100
	raw.Runs = append(raw.Runs, fast)
	report = BuildReport(raw)
	if !report.OK() || report.Rows[0].Contenders[0].Runs != 3 || *report.Rows[0].Ratios[0].Speedup != 2 {
		t.Errorf("a passing warmup changed the figures: %+v", report.Rows[0])
	}
}
