// Command lzma-turbo-bench measures lzma-turbo against its oracles (xz,
// 7-Zip, the LZMA SDK's 7lzma, lzma-rust2 and liblzma), one process per
// measurement, with each process's peak RSS, and writes a report a fleet can
// collect and merge across machines.
//
//	lzma-turbo-bench fixtures  [--verify-only]
//	lzma-turbo-bench toolchain [--build] [--fetch-7zz]
//	lzma-turbo-bench run       [--profile quick|full|fleet] [--quick] [--repeats N] [--out DIR] [--machine LABEL] ...
//	lzma-turbo-bench report    --input raw.json [--out report.json] [--md report.md]
//	lzma-turbo-bench merge     --out merged.md report.json...
//
// Exit status: 0 success, 1 a measurement or fixture failed, 2 usage, 3 the
// host is not ready (no lzma-bench build, unreadable checkout).
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"time"

	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/harness"
	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/hostinfo"
)

const (
	exitOK        = 0
	exitFailure   = 1
	exitUsage     = 2
	exitPreflight = 3
)

const usage = `usage: lzma-turbo-bench <command> [flags]

commands:
  fixtures   generate the benchmark fixtures and verify them against
             bench/fixtures/README.md (--verify-only builds nothing)
  toolchain  print the versions and provenance of everything a row depends on
             (--build builds lzma-bench first, --fetch-7zz installs the pinned
             official 7-Zip)
  run        run the matrix and write raw.json, report.json and report.md
  report     regenerate report.json and report.md from a raw.json
  merge      combine several hosts' report.json into one report.md

Run "lzma-turbo-bench <command> -h" for a command's flags.
`

func main() {
	os.Exit(dispatch(os.Args[1:]))
}

func logf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "lzma-turbo-bench: "+format+"\n", args...)
}

func dispatch(args []string) int {
	if len(args) == 0 {
		fmt.Fprint(os.Stderr, usage)
		return exitUsage
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	switch args[0] {
	case "fixtures":
		return cmdFixtures(ctx, args[1:])
	case "toolchain":
		return cmdToolchain(ctx, args[1:])
	case "run":
		return cmdRun(ctx, args[1:])
	case "report":
		return cmdReport(args[1:])
	case "merge":
		return cmdMerge(args[1:])
	case "-h", "--help", "help":
		fmt.Print(usage)
		return exitOK
	}
	fmt.Fprintf(os.Stderr, "lzma-turbo-bench: unknown command %q\n\n%s", args[0], usage)
	return exitUsage
}

type common struct {
	repo, fixtures, lzmaBench string
}

func (c *common) register(fs *flag.FlagSet) {
	fs.StringVar(&c.repo, "repo", "", "lzma-turbo checkout (default: found from the working directory or this binary)")
	fs.StringVar(&c.fixtures, "fixtures", "", "fixture directory (default: <repo>/bench/fixtures)")
	fs.StringVar(&c.lzmaBench, "lzma-bench", "", "lzma-bench release binary (default: $LZMA_TURBO_BENCH_BIN or <repo>/target/release/lzma-bench)")
}

func (c *common) paths() (harness.Paths, int) {
	paths, err := harness.ResolvePaths(c.repo, c.fixtures, c.lzmaBench)
	if err != nil {
		logf("%v", err)
		return paths, exitPreflight
	}
	return paths, exitOK
}

func parse(fs *flag.FlagSet, args []string) int {
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return -1
		}
		return exitUsage
	}
	return exitOK
}

func cmdFixtures(ctx context.Context, args []string) int {
	fs := flag.NewFlagSet("fixtures", flag.ContinueOnError)
	var c common
	c.register(fs)
	verifyOnly := fs.Bool("verify-only", false, "check the fixtures that exist; build nothing")
	asJSON := fs.Bool("json", false, "print the statuses as JSON")
	if rc := parse(fs, args); rc != exitOK {
		return max(rc, 0)
	}
	paths, rc := c.paths()
	if rc != exitOK {
		return rc
	}
	tc, err := harness.ResolveToolchain(ctx, paths, harness.ToolchainOptions{Log: logf})
	if err != nil {
		logf("%v", err)
		return exitPreflight
	}
	statuses, err := harness.Fixtures(ctx, paths, tc, harness.FixtureOptions{VerifyOnly: *verifyOnly, Log: logf})
	if err != nil {
		logf("%v", err)
		return exitFailure
	}
	bad := 0
	if *asJSON {
		data, _ := json.MarshalIndent(statuses, "", "  ")
		fmt.Println(string(data))
	}
	for _, s := range statuses {
		size := "-"
		if s.Digest != nil {
			size = fmt.Sprint(s.Digest.Size)
		}
		if !*asJSON {
			fmt.Printf("%-9s %-22s %12s  %s\n", s.Status, s.Name, size, s.Reason)
		}
		if s.Status == "missing" || s.Status == "mismatch" {
			bad++
		}
	}
	if bad > 0 {
		logf("%d fixture(s) missing or wrong", bad)
		return exitFailure
	}
	return exitOK
}

func cmdToolchain(ctx context.Context, args []string) int {
	fs := flag.NewFlagSet("toolchain", flag.ContinueOnError)
	var c common
	c.register(fs)
	build := fs.Bool("build", false, "cargo build --release --locked -p lzma-bench first")
	fetch := fs.Bool("fetch-7zz", false, "install the pinned official 7-Zip with cargo xtask sevenzip when none resolves")
	if rc := parse(fs, args); rc != exitOK {
		return max(rc, 0)
	}
	paths, rc := c.paths()
	if rc != exitOK {
		return rc
	}
	tc, err := harness.ResolveToolchain(ctx, paths, harness.ToolchainOptions{Build: *build, Fetch7zz: *fetch, Log: logf})
	if err != nil {
		logf("%v", err)
		return exitPreflight
	}
	data, _ := json.MarshalIndent(tc, "", "  ")
	fmt.Println(string(data))
	if !tc.LzmaBench.Found() {
		logf("%s", tc.LzmaBench.Missing)
		return exitPreflight
	}
	return exitOK
}

func cmdRun(ctx context.Context, args []string) int {
	fs := flag.NewFlagSet("run", flag.ContinueOnError)
	var c common
	c.register(fs)
	out := fs.String("out", "", "output directory (default: <repo>/bench/results/<machine>-<UTC time>)")
	machine := fs.String("machine", "", "host label in the reports (default: <os>-<arch>-<cpus>cpu)")
	profile := fs.String("profile", "", "matrix profile: quick (smoke: p256, threads 1,all, presets 1,5, MT preset 5, 1 repeat), "+
		"full (every sweep, p256 and 1 GiB, 5 repeats; the default) or "+
		"fleet (quick's rows plus presets 1,5,9, an MT encode sweep 2,4,8,all at preset 5 and one 1 GiB decode row, 3 repeats, 1 warmup)")
	quick := fs.Bool("quick", false, "the same as --profile quick")
	repeats := fs.Int("repeats", 0, "measured repeats per contender (default: the profile's, 5 full, 3 fleet, 1 quick)")
	warmups := fs.Int("warmups", -1, "unmeasured warmup passes per scenario (default: the profile's, 1 full and fleet, 0 quick)")
	threads := fs.String("threads", "", "thread sweep, e.g. 1,2,4,8,16,all")
	presets := fs.String("presets", "", "single-thread encode presets, e.g. 1,3,5,6,9")
	mtPresets := fs.String("mt-presets", "", "multi-threaded encode presets, e.g. 5,6")
	sizes := fs.String("sizes", "", "payload sizes: p256 (256 MiB), 1g (1 GiB)")
	only := fs.String("only", "", "comma-separated scenario prefixes or globs, e.g. decode/7z-mt,encode/st/*")
	timeout := fs.Duration("timeout", 60*time.Minute, "bound on one process")
	build := fs.Bool("build", false, "cargo build --release --locked -p lzma-bench first")
	list := fs.Bool("list", false, "print the selected scenarios and exit")
	if rc := parse(fs, args); rc != exitOK {
		return max(rc, 0)
	}
	if *quick {
		if *profile != "" && *profile != harness.ProfileQuick {
			logf("--quick and --profile %s disagree", *profile)
			return exitUsage
		}
		*profile = harness.ProfileQuick
	}
	if *profile == "" {
		*profile = harness.ProfileFull
	}
	defaults, err := harness.ProfileOptions(*profile)
	if err != nil {
		logf("%v", err)
		return exitUsage
	}
	matrix := defaults.Matrix
	if *threads != "" {
		sweep, err := harness.ParseInts(*threads)
		if err != nil {
			logf("%v", err)
			return exitUsage
		}
		matrix.SetThreads(sweep)
	}
	for _, f := range []struct {
		text string
		into *[]int
	}{{*presets, &matrix.Presets}, {*mtPresets, &matrix.MTPresets}} {
		if f.text != "" {
			if *f.into, err = harness.ParseInts(f.text); err != nil {
				logf("%v", err)
				return exitUsage
			}
		}
	}
	if *sizes != "" {
		matrix.Sizes = splitList(*sizes)
	}
	matrix.Only = splitList(*only)
	if *repeats == 0 {
		*repeats = defaults.Repeats
	}
	if *warmups < 0 {
		*warmups = defaults.Warmups
	}
	if *repeats < 1 {
		logf("--repeats must be at least 1")
		return exitUsage
	}
	scenarios := harness.BuildMatrix(matrix)
	plan := harness.Plan(scenarios, *repeats, *warmups)
	planLine := fmt.Sprintf("profile %s: %d scenarios (%d skipped on this host), %d contender rows, %d processes at %d repeat(s) + %d warmup(s)",
		*profile, plan.Scenarios, plan.Skipped, plan.Rows, plan.Processes, *repeats, *warmups)
	if *list {
		for _, s := range scenarios {
			skip := ""
			if s.Skip != "" {
				skip = "  (skip: " + s.Skip + ")"
			}
			fmt.Printf("%s%s\n", s.ID, skip)
		}
		fmt.Println(planLine)
		return exitOK
	}
	if len(scenarios) == 0 {
		logf("--only selected no scenarios")
		return exitUsage
	}

	paths, rc := c.paths()
	if rc != exitOK {
		return rc
	}
	tc, err := harness.ResolveToolchain(ctx, paths, harness.ToolchainOptions{Build: *build, Log: logf})
	if err != nil {
		logf("%v", err)
		return exitPreflight
	}
	if !tc.LzmaBench.Found() {
		logf("%s", tc.LzmaBench.Missing)
		return exitPreflight
	}
	host := hostinfo.Collect(ctx, *machine)
	dir := *out
	if dir == "" {
		dir = filepath.Join(paths.Repo, "bench", "results", host.Label+"-"+time.Now().UTC().Format("20060102T150405Z"))
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		logf("%v", err)
		return exitPreflight
	}
	journal, err := os.Create(filepath.Join(dir, "runs.jsonl"))
	if err != nil {
		logf("%v", err)
		return exitPreflight
	}
	defer journal.Close()
	logf("host %s: %s, %d CPUs, tier %s; writing %s", host.Label, host.CPU, host.CPUCount, host.Tier, dir)
	logf("%s", planLine)

	settings := harness.RunSettings{Profile: *profile, Quick: *profile == harness.ProfileQuick, Repeats: *repeats, Warmups: *warmups, Threads: matrix.Threads,
		Presets: matrix.Presets, MTPresets: matrix.MTPresets, Sizes: matrix.Sizes, Only: matrix.Only,
		TimeoutSeconds: timeout.Seconds(), CommandLine: strings.Join(append([]string{"lzma-turbo-bench", "run"}, args...), " ")}
	raw, runErr := harness.Execute(ctx, harness.RunOptions{Paths: paths, Toolchain: tc, Host: host, Matrix: scenarios,
		Settings: settings, Timeout: *timeout, Journal: journal, Log: logf})
	if raw == nil {
		logf("%v", runErr)
		return exitFailure
	}
	if raw.Finished == "" {
		raw.Finished = time.Now().UTC().Format(time.RFC3339)
	}
	if err := harness.WriteJSON(filepath.Join(dir, "raw.json"), raw); err != nil {
		logf("%v", err)
		return exitFailure
	}
	report := harness.BuildReport(raw)
	if err := harness.WriteReports(report, filepath.Join(dir, "report.json"), filepath.Join(dir, "report.md")); err != nil {
		logf("%v", err)
		return exitFailure
	}
	logf("wrote %s", filepath.Join(dir, "report.md"))
	if runErr != nil {
		logf("interrupted: %v", runErr)
		return exitFailure
	}
	if !report.OK() {
		for _, f := range report.Failures {
			logf("FAILED %s", f)
		}
		return exitFailure
	}
	return exitOK
}

func cmdReport(args []string) int {
	fs := flag.NewFlagSet("report", flag.ContinueOnError)
	input := fs.String("input", "", "raw.json from a run")
	out := fs.String("out", "", "report.json to write (default: beside the input)")
	md := fs.String("md", "", "report.md to write (default: beside --out)")
	if rc := parse(fs, args); rc != exitOK {
		return max(rc, 0)
	}
	if *input == "" {
		logf("report needs --input raw.json")
		return exitUsage
	}
	raw, err := harness.ReadRaw(*input)
	if err != nil {
		logf("%v", err)
		return exitFailure
	}
	if *out == "" {
		*out = filepath.Join(filepath.Dir(*input), "report.json")
	}
	if *md == "" {
		*md = strings.TrimSuffix(*out, filepath.Ext(*out)) + ".md"
	}
	report := harness.BuildReport(raw)
	if err := harness.WriteReports(report, *out, *md); err != nil {
		logf("%v", err)
		return exitFailure
	}
	if !report.OK() {
		return exitFailure
	}
	return exitOK
}

func cmdMerge(args []string) int {
	fs := flag.NewFlagSet("merge", flag.ContinueOnError)
	out := fs.String("out", "", "merged report.md to write (default: stdout)")
	if rc := parse(fs, args); rc != exitOK {
		return max(rc, 0)
	}
	if fs.NArg() == 0 {
		logf("merge needs one or more report.json files")
		return exitUsage
	}
	var reports []*harness.Report
	for _, path := range fs.Args() {
		report, err := harness.ReadReport(path)
		if err != nil {
			logf("%v", err)
			return exitFailure
		}
		reports = append(reports, report)
	}
	text := harness.MergeMarkdown(reports)
	if *out == "" {
		fmt.Print(text)
		return exitOK
	}
	if err := os.WriteFile(*out, []byte(text), 0o644); err != nil {
		logf("%v", err)
		return exitFailure
	}
	return exitOK
}

func splitList(text string) []string {
	var out []string
	for _, part := range strings.Split(text, ",") {
		if part = strings.TrimSpace(part); part != "" {
			out = append(out, part)
		}
	}
	return out
}
