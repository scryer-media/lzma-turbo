package harness

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"time"
)

// Tool is one external program the harness resolved, and how.
type Tool struct {
	Name       string `json:"name"`
	Path       string `json:"path,omitempty"`
	Version    string `json:"version,omitempty"`
	SHA256     string `json:"sha256,omitempty"`
	Provenance string `json:"provenance,omitempty"`
	// Missing says why the tool was not found; its rows are skipped with
	// this as the reason.
	Missing string `json:"missing,omitempty"`
	// Filters is, for xz, the filters this build can write.
	Filters []string `json:"filters,omitempty"`
}

// Found reports whether the tool resolved.
func (t Tool) Found() bool { return t.Path != "" && t.Missing == "" }

// BuildInfo is what `lzma-bench --shot info` reports about the build: which
// decode loop it compiled, and which CRC tier crc-fast picked on this host.
type BuildInfo struct {
	LzmaTurbo             string   `json:"lzma_turbo"`
	AsmLoop               bool     `json:"asm_loop"`
	TargetArch            string   `json:"target_arch"`
	TargetOS              string   `json:"target_os"`
	CRC32Tier             string   `json:"crc32_tier"`
	CRC64Tier             string   `json:"crc64_tier"`
	SHA256Backend         string   `json:"sha256_backend"`
	CompileTargetFeatures []string `json:"compile_target_features"`
}

// Crate is the checkout being measured.
type Crate struct {
	Version string `json:"version"`
	Commit  string `json:"commit,omitempty"`
	Dirty   bool   `json:"dirty"`
}

// Toolchain is every version a row depends on.
type Toolchain struct {
	Crate     Crate      `json:"crate"`
	Rustc     string     `json:"rustc,omitempty"`
	Cargo     string     `json:"cargo,omitempty"`
	Go        string     `json:"go"`
	LzmaBench Tool       `json:"lzma_bench"`
	Build     *BuildInfo `json:"build,omitempty"`
	XZ        Tool       `json:"xz"`
	SevenZip  Tool       `json:"sevenzip"`
	SevenLZMA Tool       `json:"7lzma"`
	// Crates are the versions Cargo.lock pins for the in-process
	// references and the libraries whose dispatch the report names.
	Crates map[string]string `json:"crates"`
}

// ToolchainOptions configures resolution.
type ToolchainOptions struct {
	// Build runs `cargo build --release --locked -p lzma-bench` first.
	Build bool
	// Fetch7zz installs the pinned official 7-Zip with `cargo xtask
	// sevenzip` when no 7zz resolves.
	Fetch7zz bool
	Log      func(format string, args ...any)
}

// The crates the report names, from Cargo.lock.
var lockedCrates = []string{"lzma-rust2", "liblzma", "liblzma-sys", "crc-fast", "aws-lc-rs", "aws-lc-sys"}

// xzFilters are the xz options probed, by the name the harness uses.
var xzFilters = []struct{ name, option string }{
	{"x86", "--x86"}, {"arm", "--arm"}, {"armthumb", "--armthumb"}, {"arm64", "--arm64"},
	{"ppc", "--powerpc"}, {"sparc", "--sparc"}, {"ia64", "--ia64"}, {"riscv", "--riscv"},
	{"delta", "--delta=dist=4"},
}

// XZFilterOption is the xz command-line option for a filter name.
func XZFilterOption(name string) string {
	if dist, ok := strings.CutPrefix(name, "delta:"); ok {
		return "--delta=dist=" + dist
	}
	for _, f := range xzFilters {
		if f.name == name {
			return f.option
		}
	}
	return ""
}

// ResolveToolchain records every version, building lzma-bench first if asked.
func ResolveToolchain(ctx context.Context, paths Paths, options ToolchainOptions) (Toolchain, error) {
	logf := options.Log
	if logf == nil {
		logf = func(string, ...any) {}
	}
	tc := Toolchain{Go: runtime.Version(), Crates: map[string]string{}}
	tc.Rustc = output(ctx, paths.Repo, "rustc", "-V")
	tc.Cargo = output(ctx, paths.Repo, "cargo", "-V")
	tc.Crate.Version = crateVersion(paths.Repo)
	tc.Crate.Commit = output(ctx, paths.Repo, "git", "rev-parse", "HEAD")
	if tc.Crate.Commit != "" {
		tc.Crate.Dirty = output(ctx, paths.Repo, "git", "status", "--porcelain", "--untracked-files=no") != ""
	}
	if lock, err := os.ReadFile(filepath.Join(paths.Repo, "Cargo.lock")); err == nil {
		for _, name := range lockedCrates {
			if version := lockedVersion(string(lock), name); version != "" {
				tc.Crates[name] = version
			}
		}
	}

	if options.Build {
		logf("building lzma-bench: cargo build --release --locked -p lzma-bench")
		cmd := exec.CommandContext(ctx, "cargo", "build", "--release", "--locked", "-p", "lzma-bench")
		cmd.Dir = paths.Repo
		cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
		if err := cmd.Run(); err != nil {
			return tc, fmt.Errorf("cargo build --release --locked -p lzma-bench: %w", err)
		}
	}
	tc.LzmaBench = Tool{Name: "lzma-bench", Path: paths.LzmaBench, Provenance: "built from this checkout (tools/lzma-bench)"}
	if _, err := os.Stat(paths.LzmaBench); err != nil {
		tc.LzmaBench.Missing = "not built: run `lzma-turbo-bench toolchain --build` or `cargo build --release --locked -p lzma-bench`"
	} else {
		tc.LzmaBench.SHA256 = fileSHA256(paths.LzmaBench)
		var info BuildInfo
		text := output(ctx, paths.Repo, paths.LzmaBench, "--shot", "info")
		if err := json.Unmarshal([]byte(lastLine(text)), &info); err != nil {
			tc.LzmaBench.Missing = "predates --shot (rebuild it): " + firstLine(text)
		} else {
			tc.Build = &info
			tc.LzmaBench.Version = "lzma-turbo " + info.LzmaTurbo
		}
	}

	tc.XZ = resolveXZ(ctx)
	tc.SevenZip = resolve7zz(ctx, paths)
	if !tc.SevenZip.Found() && options.Fetch7zz {
		logf("fetching the pinned official 7-Zip: cargo xtask sevenzip")
		cmd := exec.CommandContext(ctx, "cargo", "xtask", "sevenzip")
		cmd.Dir = paths.Repo
		cmd.Stderr = os.Stderr
		if out, err := cmd.Output(); err != nil {
			logf("cargo xtask sevenzip failed: %v", err)
		} else if path := lastLine(string(out)); path != "" {
			tc.SevenZip = describe7zz(ctx, path, "cargo xtask sevenzip (pinned official release)")
		}
	}
	tc.SevenLZMA = resolve7lzma(ctx)
	return tc, nil
}

func output(ctx context.Context, dir, program string, args ...string) string {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()
	cmd := exec.CommandContext(ctx, program, args...)
	cmd.Dir = dir
	out, err := cmd.Output()
	if err != nil && len(out) == 0 {
		return ""
	}
	return strings.TrimSpace(string(out))
}

// combined runs a program for its banner, which some tools print on a usage
// error and to stderr.
func combined(ctx context.Context, program string, args ...string) string {
	ctx, cancel := context.WithTimeout(ctx, time.Minute)
	defer cancel()
	out, _ := exec.CommandContext(ctx, program, args...).CombinedOutput()
	return string(out)
}

var packageVersion = regexp.MustCompile(`(?m)^version\s*=\s*"([^"]+)"`)

func crateVersion(repo string) string {
	data, err := os.ReadFile(filepath.Join(repo, "Cargo.toml"))
	if err != nil {
		return ""
	}
	if m := packageVersion.FindSubmatch(data); m != nil {
		return string(m[1])
	}
	return ""
}

// lockedVersion is the first version Cargo.lock lists for a package name.
func lockedVersion(lock, name string) string {
	scanner := bufio.NewScanner(strings.NewReader(lock))
	want := fmt.Sprintf("name = %q", name)
	for scanner.Scan() {
		if strings.TrimSpace(scanner.Text()) != want {
			continue
		}
		if scanner.Scan() {
			if m := packageVersion.FindStringSubmatch(scanner.Text()); m != nil {
				return m[1]
			}
		}
	}
	return ""
}

func fileSHA256(path string) string {
	digest, err := DigestFile(path)
	if err != nil {
		return ""
	}
	return digest.SHA256
}

func lookPath(names ...string) (string, string) {
	for _, name := range names {
		if path, err := exec.LookPath(name); err == nil {
			if abs, err := filepath.Abs(path); err == nil {
				path = abs
			}
			return path, name
		}
	}
	return "", ""
}

func resolveXZ(ctx context.Context) Tool {
	tool := Tool{Name: "xz"}
	path, provenance := os.Getenv("LZMA_TURBO_XZ"), "env LZMA_TURBO_XZ"
	if path == "" {
		path, _ = lookPath("xz")
		provenance = "PATH"
	}
	if path == "" {
		tool.Missing = "xz not found (install XZ Utils or set LZMA_TURBO_XZ)"
		return tool
	}
	tool.Path, tool.Provenance = path, provenance
	tool.Version = firstLine(combined(ctx, path, "--version"))
	tool.SHA256 = fileSHA256(path)
	for _, f := range xzFilters {
		probe := exec.CommandContext(ctx, path, "--format=xz", "-T1", f.option, "--lzma2=preset=0", "-c")
		probe.Stdin = bytes.NewReader(bytes.Repeat([]byte("lzma-turbo-bench"), 64))
		if err := probe.Run(); err == nil {
			tool.Filters = append(tool.Filters, f.name)
		}
	}
	return tool
}

// resolve7zz prefers an explicit binary, then the pinned official release
// `cargo xtask sevenzip` installs, then PATH. On Windows the official console
// binaries are 7z.exe and 7za.exe, so those are accepted there; elsewhere a
// `7z` on PATH is usually p7zip, a fork frozen at 16.02, and is not.
func resolve7zz(ctx context.Context, paths Paths) Tool {
	if path := os.Getenv("LZMA_TURBO_7ZZ"); path != "" {
		return describe7zz(ctx, path, "env LZMA_TURBO_7ZZ")
	}
	pinned := filepath.Join(paths.Repo, "target", "sevenzip", "7zz")
	if runtime.GOOS == "windows" {
		pinned = filepath.Join(paths.Repo, "target", "sevenzip", "x64", "7za.exe")
	}
	if _, err := os.Stat(pinned); err == nil {
		return describe7zz(ctx, pinned, "cargo xtask sevenzip (pinned official release)")
	}
	names := []string{"7zz"}
	if runtime.GOOS == "windows" {
		names = []string{"7zz", "7z", "7za"}
	}
	if path, _ := lookPath(names...); path != "" {
		return describe7zz(ctx, path, "PATH")
	}
	if runtime.GOOS == "windows" {
		standard := filepath.Join(os.Getenv("ProgramFiles"), "7-Zip", "7z.exe")
		if _, err := os.Stat(standard); err == nil {
			return describe7zz(ctx, standard, "7-Zip installer default location")
		}
	}
	return Tool{Name: "7zz", Missing: "7zz not found (set LZMA_TURBO_7ZZ, put the official 7-Zip on PATH, or run `toolchain --fetch-7zz`)"}
}

func describe7zz(ctx context.Context, path, provenance string) Tool {
	tool := Tool{Name: "7zz", Path: path, Provenance: provenance}
	if _, err := os.Stat(path); err != nil {
		tool.Missing = fmt.Sprintf("%s: %v", path, err)
		return tool
	}
	tool.Version = firstLine(combined(ctx, path, "i"))
	tool.SHA256 = fileSHA256(path)
	return tool
}

func resolve7lzma(ctx context.Context) Tool {
	tool := Tool{Name: "7lzma"}
	path, provenance := os.Getenv("LZMA_TURBO_7LZMA"), "env LZMA_TURBO_7LZMA"
	if path == "" {
		path, _ = lookPath("7lzma")
		provenance = "PATH"
	}
	if path == "" {
		tool.Missing = "7lzma not found (build it from the LZMA SDK's C/Util/Lzma and set LZMA_TURBO_7LZMA; see docs/benchmarking.md)"
		return tool
	}
	tool.Path, tool.Provenance = path, provenance
	tool.Version = firstLine(combined(ctx, path))
	tool.SHA256 = fileSHA256(path)
	return tool
}
