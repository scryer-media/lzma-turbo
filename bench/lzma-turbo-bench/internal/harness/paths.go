// Package harness is lzma-turbo-bench: the fixtures, the toolchain record,
// the scenario matrix, the interleaved runner, and the reports.
package harness

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
)

// Paths are where the harness finds the repository and what it built.
type Paths struct {
	// Repo is the lzma-turbo checkout: the directory holding the crate's
	// Cargo.toml.
	Repo string
	// Fixtures is bench/fixtures under Repo unless overridden.
	Fixtures string
	// LzmaBench is the release build of tools/lzma-bench.
	LzmaBench string
}

// FindRepo walks up from start to the lzma-turbo checkout.
func FindRepo(start string) (string, error) {
	dir, err := filepath.Abs(start)
	if err != nil {
		return "", err
	}
	for {
		manifest, err := os.ReadFile(filepath.Join(dir, "Cargo.toml"))
		if err == nil && bytes.Contains(manifest, []byte("name = \"lzma-turbo\"")) {
			if info, err := os.Stat(filepath.Join(dir, "tools", "lzma-bench")); err == nil && info.IsDir() {
				return dir, nil
			}
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", errors.New("not inside an lzma-turbo checkout (pass --repo)")
		}
		dir = parent
	}
}

// ResolvePaths fills the defaults: the checkout from repo (or the working
// directory, or the executable's directory), bench/fixtures, and
// target/release/lzma-bench. LZMA_TURBO_BENCH_BIN overrides the binary.
func ResolvePaths(repo, fixtures, lzmaBench string) (Paths, error) {
	var err error
	if repo == "" {
		if repo, err = FindRepo("."); err != nil {
			exe, exeErr := os.Executable()
			if exeErr != nil {
				return Paths{}, err
			}
			if repo, err = FindRepo(filepath.Dir(exe)); err != nil {
				return Paths{}, err
			}
		}
	} else if repo, err = FindRepo(repo); err != nil {
		return Paths{}, err
	}
	if fixtures == "" {
		fixtures = filepath.Join(repo, "bench", "fixtures")
	}
	if lzmaBench == "" {
		lzmaBench = os.Getenv("LZMA_TURBO_BENCH_BIN")
	}
	if lzmaBench == "" {
		lzmaBench = filepath.Join(repo, "target", "release", exe("lzma-bench"))
	}
	fixtures, _ = filepath.Abs(fixtures)
	lzmaBench, _ = filepath.Abs(lzmaBench)
	return Paths{Repo: repo, Fixtures: fixtures, LzmaBench: lzmaBench}, nil
}

func exe(name string) string {
	if runtime.GOOS == "windows" {
		return name + ".exe"
	}
	return name
}

// Digest is a file's size, SHA-256 and CRC-32 (IEEE, the CRC lzma-bench
// prints for its output).
type Digest struct {
	Name   string `json:"name"`
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
	CRC32  string `json:"crc32"`
	// ModUnixNano lets a cached digest be reused only for the same file.
	ModUnixNano int64 `json:"mod_unix_nano"`
}

// DigestFile reads a file once for both hashes.
func DigestFile(path string) (Digest, error) {
	file, err := os.Open(path)
	if err != nil {
		return Digest{}, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return Digest{}, err
	}
	sum := sha256.New()
	crc := crc32.NewIEEE()
	if _, err := io.Copy(io.MultiWriter(sum, crc), file); err != nil {
		return Digest{}, err
	}
	return Digest{
		Name:        filepath.Base(path),
		Size:        info.Size(),
		SHA256:      hex.EncodeToString(sum.Sum(nil)),
		CRC32:       fmt.Sprintf("%08x", crc.Sum32()),
		ModUnixNano: info.ModTime().UnixNano(),
	}, nil
}

// Stat is a median with its range.
type Stat struct {
	Median float64 `json:"median"`
	Min    float64 `json:"min"`
	Max    float64 `json:"max"`
	N      int     `json:"n"`
}

func stat(values []float64) Stat {
	if len(values) == 0 {
		return Stat{}
	}
	sorted := append([]float64(nil), values...)
	sort.Float64s(sorted)
	middle := len(sorted) / 2
	median := sorted[middle]
	if len(sorted)%2 == 0 {
		median = (sorted[middle-1] + sorted[middle]) / 2
	}
	return Stat{Median: median, Min: sorted[0], Max: sorted[len(sorted)-1], N: len(sorted)}
}

func seconds(s Stat) string {
	if s.N == 0 {
		return "-"
	}
	if s.N == 1 || s.Min == s.Max {
		return fmt.Sprintf("%.3f", s.Median)
	}
	return fmt.Sprintf("%.3f [%.3f–%.3f]", s.Median, s.Min, s.Max)
}

func mebibytes(s Stat) string {
	if s.N == 0 || s.Median <= 0 {
		return "-"
	}
	if s.N == 1 || s.Min == s.Max {
		return fmt.Sprintf("%.1f", s.Median/(1<<20))
	}
	return fmt.Sprintf("%.1f [%.1f–%.1f]", s.Median/(1<<20), s.Min/(1<<20), s.Max/(1<<20))
}

func dash(value string) string {
	if value == "" {
		return "-"
	}
	return value
}

func firstLine(text string) string {
	for _, line := range strings.Split(text, "\n") {
		if line = strings.TrimSpace(line); line != "" {
			return line
		}
	}
	return ""
}

func lastLine(text string) string {
	lines := strings.Split(strings.TrimSpace(text), "\n")
	for i := len(lines) - 1; i >= 0; i-- {
		if line := strings.TrimSpace(lines[i]); line != "" {
			return line
		}
	}
	return ""
}
