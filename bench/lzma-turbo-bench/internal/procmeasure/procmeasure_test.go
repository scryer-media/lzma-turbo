package procmeasure

import (
	"context"
	"os"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

// touchEnv makes the test binary a child that touches that many MiB of fresh
// memory and exits: a process whose peak resident set is known to be at least
// that large.
const touchEnv = "PROCMEASURE_TEST_TOUCH_MIB"

// printEnv makes the test binary a child that writes that many bytes to
// standard output and exits.
const printEnv = "PROCMEASURE_TEST_PRINT_BYTES"

func TestMain(m *testing.M) {
	if value := os.Getenv(touchEnv); value != "" {
		mebibytes, err := strconv.Atoi(value)
		if err != nil {
			os.Exit(2)
		}
		buffer := make([]byte, mebibytes<<20)
		for index := 0; index < len(buffer); index += 4096 {
			buffer[index] = 1
		}
		runtime.KeepAlive(buffer)
		os.Exit(0)
	}
	if value := os.Getenv(printEnv); value != "" {
		count, err := strconv.Atoi(value)
		if err != nil {
			os.Exit(2)
		}
		_, _ = os.Stdout.Write([]byte(strings.Repeat("x", count)))
		os.Exit(0)
	}
	os.Exit(m.Run())
}

const touchedMiB = 96

func self(t *testing.T) string {
	t.Helper()
	path, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	return path
}

// The peak a child reaches is what Run records, tagged with this platform's
// source.
func TestRunRecordsTheChildsPeak(t *testing.T) {
	result := Run(context.Background(), Command{Path: self(t), Env: []string{touchEnv + "=" + strconv.Itoa(touchedMiB)}})
	if result.Failure != "" || result.ExitCode != 0 {
		t.Fatalf("child failed: %s exit %d: %v %s", result.Failure, result.ExitCode, result.Err, result.Stderr)
	}
	if result.MaxRSSBytes < touchedMiB<<20 {
		t.Fatalf("peak %d bytes, want at least %d MiB", result.MaxRSSBytes, touchedMiB)
	}
	if result.RSSSource != NativeRSSSource {
		t.Fatalf("rss source %q, want %q", result.RSSSource, NativeRSSSource)
	}
}

// A run that never started carries no peak and no source.
func TestAStartFailureHasNoPeak(t *testing.T) {
	result := Run(context.Background(), Command{Path: "lzma-turbo-bench-no-such-program"})
	if result.Failure != "start-failed" || result.MaxRSSBytes != 0 || result.RSSSource != "" {
		t.Fatalf("got failure %q peak %d source %q", result.Failure, result.MaxRSSBytes, result.RSSSource)
	}
}

// A writer in Command.Stdout receives the child's output instead of the
// captured tail.
func TestStdoutCanBeCounted(t *testing.T) {
	var counter CountingWriter
	result := Run(context.Background(), Command{Path: self(t), Env: []string{printEnv + "=100000"}, Stdout: &counter})
	if result.Failure != "" || result.ExitCode != 0 {
		t.Fatalf("child failed: %s exit %d", result.Failure, result.ExitCode)
	}
	if counter.N != 100000 || result.Stdout != "" {
		t.Fatalf("counted %d, tail %d bytes", counter.N, len(result.Stdout))
	}
}

func TestPeakStatsAndSort(t *testing.T) {
	median, low, high := PeakStats([]int64{30, 10, 20, 40})
	if median != 25 || low != 10 || high != 40 {
		t.Fatalf("got %d %d %d", median, low, high)
	}
	one, two := 1.5, 0.5
	scenarios := []RSSScenario{{Scenario: "b", Ratio: &two}, {Scenario: "c"}, {Scenario: "a", Ratio: &one}}
	SortRSSScenarios(scenarios)
	// Reference/ours: the lowest ratio is lzma-turbo's worst peak, so it leads.
	if scenarios[0].Scenario != "b" || scenarios[1].Scenario != "a" || scenarios[2].Scenario != "c" {
		t.Fatalf("order %v %v %v", scenarios[0].Scenario, scenarios[1].Scenario, scenarios[2].Scenario)
	}
	scenario := RSSScenario{OursMedianBytes: 2, ReferenceMedianBytes: 3, OursSource: "rusage", ReferenceSource: "rusage"}
	CompleteRSSScenario(&scenario)
	if scenario.Ratio == nil || *scenario.Ratio != 1.5 || scenario.Note != "" {
		t.Fatalf("ratio %v note %q", scenario.Ratio, scenario.Note)
	}
}
