// Package procmeasure measures one child process: wall time from the parent's
// monotonic clock, and CPU time, peak resident set and I/O counters from the
// kernel's accounting of the exited child.
//
// It is a copy of rarpar-bench's procmeasure (github.com/scryer-media/rarpar,
// bench/rarpar-bench/internal/procmeasure), trimmed to what this harness uses
// and kept to the same JSON field names, so a fleet runner that reads one
// harness's rows reads the other's. It is copied rather than imported because
// the two harnesses live in different repositories.
package procmeasure

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"
)

// Measurement is what one timed process run costs. Wall time is the parent's
// monotonic clock around start-to-exit; CPU and memory come from the kernel's
// accounting of the exited child.
type Measurement struct {
	WallSeconds float64 `json:"wall_seconds"`
	UserSeconds float64 `json:"user_seconds"`
	SysSeconds  float64 `json:"sys_seconds"`
	// MaxRSSBytes is the peak resident set, required on every ok row: the
	// child's own rusage ru_maxrss on macOS (bytes) and Linux (KiB,
	// normalised), and PeakWorkingSetSize from K32GetProcessMemoryInfo on a
	// handle held across the child's exit on Windows.
	MaxRSSBytes int64 `json:"max_rss_bytes"`
	// RSSSource names where MaxRSSBytes came from (one of the RSSSource*
	// constants). Empty when no peak was captured.
	RSSSource string `json:"rss_source,omitempty"`
	// BlockInOps / BlockOutOps are ru_inblock / ru_oublock (POSIX).
	BlockInOps  int64 `json:"block_in_ops,omitempty"`
	BlockOutOps int64 `json:"block_out_ops,omitempty"`
	// Windows GetProcessIoCounters.
	ReadOps    int64 `json:"read_ops,omitempty"`
	WriteOps   int64 `json:"write_ops,omitempty"`
	ReadBytes  int64 `json:"read_bytes,omitempty"`
	WriteBytes int64 `json:"write_bytes,omitempty"`
	ExitCode   int   `json:"exit_code"`
}

// Command is one process to run and measure.
type Command struct {
	Path string
	Args []string
	Dir  string
	// Env entries are appended to the harness's own environment.
	Env []string
	// Stdout, when set, receives the child's standard output instead of the
	// captured tail. An *os.File (os.DevNull, say) is handed to the child
	// directly, with no pipe; any other writer is fed through a pipe.
	Stdout io.Writer
	// Timeout bounds the run; zero means no bound beyond the context.
	Timeout time.Duration
}

// Result is a finished run: its measurement, its captured output tails and a
// classified failure, if any.
type Result struct {
	Measurement
	Stdout string
	Stderr string
	// Failure is empty on a clean exit status the caller then judges, or a
	// class: "start-failed", "timeout", "signal".
	Failure string
	Err     error
}

const outputTail = 4096

// killWaitDelay bounds how long Wait lingers for the output pipes after the
// child exits or is killed.
const killWaitDelay = 5 * time.Second

func tail(buffer *bytes.Buffer) string {
	data := buffer.Bytes()
	if len(data) > outputTail {
		data = data[len(data)-outputTail:]
	}
	return string(data)
}

// Run executes a command and measures it.
func Run(ctx context.Context, command Command) Result {
	if command.Timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, command.Timeout)
		defer cancel()
	}
	cmd := exec.CommandContext(ctx, command.Path, command.Args...)
	cmd.Dir = command.Dir
	cmd.Env = append(os.Environ(), command.Env...)
	// A timeout must end the whole tree, not only the direct child.
	configureKill(cmd)
	cmd.WaitDelay = killWaitDelay
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	if command.Stdout != nil {
		cmd.Stdout = command.Stdout
	}
	cmd.Stderr = &stderr

	started := time.Now()
	if err := cmd.Start(); err != nil {
		return Result{Err: err, Failure: "start-failed"}
	}
	probe := attachProbe(cmd)
	waitErr := cmd.Wait()
	wall := time.Since(started)

	result := Result{Stdout: tail(&stdout), Stderr: tail(&stderr)}
	result.WallSeconds = wall.Seconds()
	if state := cmd.ProcessState; state != nil {
		result.UserSeconds = state.UserTime().Seconds()
		result.SysSeconds = state.SystemTime().Seconds()
		result.ExitCode = state.ExitCode()
		fillRusage(&result.Measurement, state)
	}
	probe.finish(&result.Measurement)
	if result.MaxRSSBytes > 0 {
		result.RSSSource = NativeRSSSource
	}
	if ctx.Err() != nil {
		result.Failure = "timeout"
		result.Err = ctx.Err()
		return result
	}
	if errors.Is(waitErr, exec.ErrWaitDelay) {
		waitErr = nil
	}
	if waitErr != nil {
		var exitErr *exec.ExitError
		if errors.As(waitErr, &exitErr) {
			if result.ExitCode < 0 {
				result.Failure = "signal"
				result.Err = waitErr
			}
			return result
		}
		result.Failure = "start-failed"
		result.Err = waitErr
	}
	return result
}

// Describe renders a command line for logs and evidence.
func (command Command) Describe() string {
	parts := append([]string{command.Path}, command.Args...)
	for i, part := range parts {
		if strings.ContainsAny(part, " \t\"'") {
			parts[i] = fmt.Sprintf("%q", part)
		}
	}
	return strings.Join(parts, " ")
}

// CountingWriter discards what it is given and counts it: the size of an
// output nobody needs to keep, such as an oracle encoder's stream.
type CountingWriter struct{ N int64 }

func (w *CountingWriter) Write(p []byte) (int, error) {
	w.N += int64(len(p))
	return len(p), nil
}
