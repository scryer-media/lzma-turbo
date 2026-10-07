package harness

import (
	"context"
	"encoding/json"
	"fmt"
	"hash/crc32"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/hostinfo"
	"github.com/scryer-media/lzma-turbo/bench/lzma-turbo-bench/internal/procmeasure"
)

// Run statuses.
const (
	StatusOK      = "ok"
	StatusFailed  = "failed"
	StatusDNF     = "dnf"
	StatusSkipped = "skipped"
)

// Raw schema identity.
const (
	RawSchema     = "lzma-turbo-bench/raw"
	SchemaVersion = 1
)

// ShotLine is the JSON line `lzma-bench --shot` prints.
type ShotLine struct {
	Lane               string  `json:"lane"`
	Threads            int     `json:"threads"`
	Preset             int     `json:"preset"`
	Filter             string  `json:"filter"`
	Direction          string  `json:"direction"`
	BytesIn            int64   `json:"bytes_in"`
	BytesOut           int64   `json:"bytes_out"`
	CRC32              string  `json:"crc32"`
	InprocSeconds      float64 `json:"inproc_seconds"`
	PeakAllocBytes     int64   `json:"peak_alloc_bytes"`
	InputBufferedBytes int64   `json:"input_buffered_bytes"`
}

// RunRecord is one measured process.
type RunRecord struct {
	Scenario  string `json:"scenario"`
	Contender string `json:"contender"`
	Role      string `json:"role"`
	Repeat    int    `json:"repeat"`
	Warmup    bool   `json:"warmup,omitempty"`
	// Position is the contender's place in this repeat's order.
	Position int    `json:"position"`
	Status   string `json:"status"`
	Failure  string `json:"failure,omitempty"`
	Reason   string `json:"reason,omitempty"`
	// LoadBefore is the one-minute load average read just before the run,
	// or -1 where the OS has none.
	LoadBefore float64 `json:"load_before"`
	Command    string  `json:"command"`
	procmeasure.Measurement
	BytesIn  int64 `json:"bytes_in,omitempty"`
	BytesOut int64 `json:"bytes_out,omitempty"`
	// CRC32 is the shot's output CRC; ExpectedCRC32 what a decode must give.
	CRC32         string    `json:"crc32,omitempty"`
	ExpectedCRC32 string    `json:"expected_crc32,omitempty"`
	Shot          *ShotLine `json:"shot,omitempty"`
	Stderr        string    `json:"stderr,omitempty"`
}

// SkippedScenario is a row the host could not run, and why.
type SkippedScenario struct {
	Scenario string `json:"scenario"`
	Reason   string `json:"reason"`
}

// DroppedContender is a reference a scenario ran without.
type DroppedContender struct {
	Scenario  string `json:"scenario"`
	Contender string `json:"contender"`
	Reason    string `json:"reason"`
}

// RunSettings are the knobs a run used, kept with its data.
type RunSettings struct {
	Quick          bool     `json:"quick"`
	Repeats        int      `json:"repeats"`
	Warmups        int      `json:"warmups"`
	Threads        []int    `json:"threads"`
	Presets        []int    `json:"presets"`
	MTPresets      []int    `json:"mt_presets"`
	Sizes          []string `json:"sizes"`
	Only           []string `json:"only,omitempty"`
	TimeoutSeconds float64  `json:"timeout_seconds"`
	CommandLine    string   `json:"command_line"`
}

// Raw is everything a run measured; report.json is derived from it.
type Raw struct {
	Schema        string             `json:"schema"`
	SchemaVersion int                `json:"schema_version"`
	Started       string             `json:"started"`
	Finished      string             `json:"finished"`
	Host          hostinfo.Host      `json:"host"`
	Toolchain     Toolchain          `json:"toolchain"`
	Settings      RunSettings        `json:"settings"`
	Fixtures      map[string]Digest  `json:"fixtures"`
	Scenarios     []Scenario         `json:"scenarios"`
	Runs          []RunRecord        `json:"runs"`
	Skipped       []SkippedScenario  `json:"skipped"`
	Dropped       []DroppedContender `json:"dropped"`
}

// RunOptions configures a run.
type RunOptions struct {
	Paths     Paths
	Toolchain Toolchain
	Host      hostinfo.Host
	Matrix    []Scenario
	Settings  RunSettings
	Timeout   time.Duration
	// Journal, when set, receives every RunRecord as a JSON line as it
	// finishes, so an interrupted run keeps what it measured.
	Journal io.Writer
	Log     func(format string, args ...any)
}

// orderFor interleaves contenders: the listed order on even repeats and the
// reverse on odd ones, so neither side always runs on a warm or a cold cache
// or in the other's thermal wake (rarpar-bench's schedule).
func orderFor(n, repeat int) []int {
	order := make([]int, n)
	for i := range order {
		if repeat%2 == 0 {
			order[i] = i
		} else {
			order[i] = n - 1 - i
		}
	}
	return order
}

// Execute runs the matrix.
func Execute(ctx context.Context, options RunOptions) (*Raw, error) {
	logf := options.Log
	if logf == nil {
		logf = func(string, ...any) {}
	}
	if options.Settings.Repeats < 1 {
		options.Settings.Repeats = 1
	}
	raw := &Raw{
		Schema: RawSchema, SchemaVersion: SchemaVersion,
		Started: time.Now().UTC().Format(time.RFC3339),
		Host:    options.Host, Toolchain: options.Toolchain, Settings: options.Settings,
		Scenarios: options.Matrix,
	}
	var names []string
	for _, s := range options.Matrix {
		names = append(names, s.Input, s.Source)
	}
	digests, err := FixtureDigests(options.Paths, names)
	if err != nil {
		return nil, err
	}
	raw.Fixtures = digests
	null, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		return nil, err
	}
	defer null.Close()
	expected := map[string]string{}

	for index, scenario := range options.Matrix {
		if ctx.Err() != nil {
			return raw, ctx.Err()
		}
		contenders, reason := prepare(options, scenario, digests, raw)
		if reason != "" {
			logf("[%d/%d] %s: skipped: %s", index+1, len(options.Matrix), scenario.ID, reason)
			raw.Skipped = append(raw.Skipped, SkippedScenario{Scenario: scenario.ID, Reason: reason})
			continue
		}
		want := ""
		if scenario.Direction == "decode" && scenario.Source != "" {
			key := fmt.Sprintf("%s*%d", scenario.Source, max(scenario.Repeat, 1))
			if want = expected[key]; want == "" {
				if want, err = repeatedCRC(filepath.Join(options.Paths.Fixtures, scenario.Source), max(scenario.Repeat, 1)); err != nil {
					return raw, err
				}
				expected[key] = want
			}
		}
		logf("[%d/%d] %s (%d contenders, %d repeats)", index+1, len(options.Matrix), scenario.ID, len(contenders), options.Settings.Repeats)
		stopped := map[string]string{}
		total := options.Settings.Warmups + options.Settings.Repeats
		for pass := 0; pass < total; pass++ {
			warmup := pass < options.Settings.Warmups
			repeat := pass - options.Settings.Warmups
			for position, i := range orderFor(len(contenders), pass) {
				contender := contenders[i]
				if why := stopped[contender.Name]; why != "" {
					continue
				}
				record := runOne(ctx, options, scenario, contender, null, want)
				record.Repeat, record.Warmup, record.Position = repeat, warmup, position
				if warmup {
					record.Repeat = -1
				}
				if options.Journal != nil {
					if line, err := json.Marshal(record); err == nil {
						_, _ = options.Journal.Write(append(line, '\n'))
					}
				}
				raw.Runs = append(raw.Runs, record)
				switch record.Status {
				case StatusDNF:
					stopped[contender.Name] = record.Reason
					logf("    %s DNF: %s", contender.Name, record.Reason)
				case StatusFailed:
					stopped[contender.Name] = record.Reason
					logf("    %s FAILED: %s", contender.Name, record.Reason)
				default:
					logf("    %s%s: %.3f s, %s MiB peak", contender.Name, map[bool]string{true: " (warmup)"}[warmup],
						timeOf(scenario, record), procmeasure.MiB(record.MaxRSSBytes))
				}
			}
			if stopped[scenario.Ours().Name] != "" {
				break
			}
		}
	}
	raw.Finished = time.Now().UTC().Format(time.RFC3339)
	return raw, nil
}

func timeOf(scenario Scenario, record RunRecord) float64 {
	if scenario.Metric == "inproc" && record.Shot != nil {
		return record.Shot.InprocSeconds
	}
	return record.WallSeconds
}

// prepare drops the references this host cannot run and says why a whole
// scenario cannot run, or "".
func prepare(options RunOptions, scenario Scenario, digests map[string]Digest, raw *Raw) ([]Contender, string) {
	if scenario.Skip != "" {
		return nil, scenario.Skip
	}
	if !options.Toolchain.LzmaBench.Found() {
		return nil, "lzma-bench: " + options.Toolchain.LzmaBench.Missing
	}
	for _, name := range []string{scenario.Input, scenario.Source} {
		if name == "" {
			continue
		}
		if _, ok := digests[name]; !ok {
			return nil, "fixture " + name + " missing (run `lzma-turbo-bench fixtures`)"
		}
	}
	var keep []Contender
	var dropped []string
	references := 0
	for _, c := range scenario.Contenders {
		if c.Role == RoleReference {
			references++
		}
		reason := ""
		switch c.Kind {
		case KindXZ:
			if !options.Toolchain.XZ.Found() {
				reason = options.Toolchain.XZ.Missing
			} else if scenario.Filter != "" {
				base := scenario.Filter
				if strings.HasPrefix(base, "delta:") {
					base = "delta"
				}
				if !contains(options.Toolchain.XZ.Filters, base) {
					reason = fmt.Sprintf("this xz (%s) has no %s filter", options.Toolchain.XZ.Version, base)
				}
			}
		case Kind7zz:
			if !options.Toolchain.SevenZip.Found() {
				reason = options.Toolchain.SevenZip.Missing
			}
		case Kind7lzma:
			if !options.Toolchain.SevenLZMA.Found() {
				reason = options.Toolchain.SevenLZMA.Missing
			}
		}
		if reason != "" {
			raw.Dropped = append(raw.Dropped, DroppedContender{Scenario: scenario.ID, Contender: c.Name, Reason: reason})
			dropped = append(dropped, c.Name+": "+reason)
			continue
		}
		keep = append(keep, c)
	}
	if references > 0 && len(keep) == 1 {
		return nil, "no reference available: " + strings.Join(dropped, "; ")
	}
	return keep, ""
}

func repeatedCRC(path string, repeat int) (string, error) {
	crc := crc32.NewIEEE()
	for i := 0; i < repeat; i++ {
		file, err := os.Open(path)
		if err != nil {
			return "", err
		}
		_, err = io.Copy(crc, file)
		file.Close()
		if err != nil {
			return "", err
		}
	}
	return fmt.Sprintf("%08x", crc.Sum32()), nil
}

// command builds the process for one contender.
func command(options RunOptions, scenario Scenario, contender Contender) procmeasure.Command {
	program := options.Paths.LzmaBench
	switch contender.Kind {
	case KindXZ:
		program = options.Toolchain.XZ.Path
	case Kind7zz:
		program = options.Toolchain.SevenZip.Path
	case Kind7lzma:
		program = options.Toolchain.SevenLZMA.Path
	}
	input := filepath.Join(options.Paths.Fixtures, scenario.Input)
	args := make([]string, len(contender.Args))
	for i, arg := range contender.Args {
		switch arg {
		case "{in}":
			args[i] = input
		case "{null}":
			args[i] = os.DevNull
		default:
			args[i] = arg
		}
	}
	return procmeasure.Command{Path: program, Args: args, Dir: options.Paths.Fixtures, Timeout: options.Timeout}
}

func runOne(ctx context.Context, options RunOptions, scenario Scenario, contender Contender, null *os.File, want string) RunRecord {
	cmd := command(options, scenario, contender)
	var counter procmeasure.CountingWriter
	switch contender.Stdout {
	case StdoutDiscard:
		cmd.Stdout = null
	case StdoutCount:
		cmd.Stdout = &counter
	}
	load, _ := hostinfo.LoadAverage()
	record := RunRecord{Scenario: scenario.ID, Contender: contender.Name, Role: contender.Role, LoadBefore: load, Command: cmd.Describe()}
	result := procmeasure.Run(ctx, cmd)
	record.Measurement = result.Measurement
	fail := func(status, failure, reason string) RunRecord {
		record.Status, record.Failure, record.Reason = status, failure, reason
		if tail := strings.TrimSpace(result.Stderr); tail != "" {
			record.Stderr = tail
		}
		// A reference that cannot finish is a did-not-finish; lzma-turbo
		// failing, or the harness failing to measure, is a failure.
		if contender.Role == RoleReference && status == StatusFailed && failure != procmeasure.FailureMissingRSS {
			record.Status = StatusDNF
		}
		return record
	}
	if result.Failure != "" {
		reason := result.Failure
		if result.Err != nil {
			reason += ": " + result.Err.Error()
		}
		return fail(StatusFailed, result.Failure, reason)
	}
	if result.ExitCode != 0 {
		return fail(StatusFailed, "exit", fmt.Sprintf("exit status %d: %s", result.ExitCode, firstLine(lastLine(result.Stderr))))
	}
	if contender.Stdout == StdoutCount {
		record.BytesOut = counter.N
	}
	if contender.Kind == KindShot {
		var line ShotLine
		if err := json.Unmarshal([]byte(lastLine(result.Stdout)), &line); err != nil {
			return fail(StatusFailed, "bad-shot-line", "no JSON line from lzma-bench: "+firstLine(result.Stdout))
		}
		record.Shot = &line
		record.BytesIn, record.BytesOut, record.CRC32 = line.BytesIn, line.BytesOut, line.CRC32
		if want != "" {
			record.ExpectedCRC32 = want
			if line.CRC32 != want {
				return fail(StatusFailed, "crc-mismatch", fmt.Sprintf("output CRC-32 %s, want %s", line.CRC32, want))
			}
		}
	}
	if result.MaxRSSBytes <= 0 || result.RSSSource == "" {
		return fail(StatusFailed, procmeasure.FailureMissingRSS, "the OS reported no peak RSS for this process")
	}
	record.Status = StatusOK
	return record
}

// WriteJSON writes v indented, through a temporary file.
func WriteJSON(path string, v any) error {
	data, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	partial := path + ".partial"
	if err := os.WriteFile(partial, append(data, '\n'), 0o644); err != nil {
		return err
	}
	return os.Rename(partial, path)
}

// ReadRaw loads a raw.json.
func ReadRaw(path string) (*Raw, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var raw Raw
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if raw.Schema != RawSchema || raw.SchemaVersion != SchemaVersion {
		return nil, fmt.Errorf("%s: schema %s v%d, want %s v%d", path, raw.Schema, raw.SchemaVersion, RawSchema, SchemaVersion)
	}
	return &raw, nil
}
