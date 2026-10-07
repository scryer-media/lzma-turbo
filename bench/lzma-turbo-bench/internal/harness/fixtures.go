package harness

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"strings"
)

// Fixture is one row of bench/fixtures/README.md: what the file must be.
type Fixture struct {
	Name string `json:"name"`
	// Size is the README's size column as written.
	Size string `json:"size"`
	// SHA256 is set only for the fixtures that are the same bytes on every
	// machine (the seeded payloads); a compressed fixture depends on the xz
	// or 7-Zip build that wrote it, so it is checked by size alone.
	SHA256 string `json:"sha256,omitempty"`
	How    string `json:"how"`
	// Extra fixtures are made by this harness rather than `cargo xtask
	// fixtures`.
	Extra bool `json:"extra"`
}

// FixtureStatus is a fixture checked against its row.
type FixtureStatus struct {
	Fixture
	Status string  `json:"status"` // ok | missing | mismatch | skipped
	Reason string  `json:"reason,omitempty"`
	Digest *Digest `json:"digest,omitempty"`
}

// sizeRule is a parsed size column.
type sizeRule struct {
	exact, approx, upTo int64
	varies              bool
}

var (
	tableRow    = regexp.MustCompile("^\\|\\s*`([^`]+)`\\s*\\|([^|]*)\\|([^|]*)\\|(.*)\\|\\s*$")
	approxSize  = regexp.MustCompile(`^~\s*([0-9.]+)\s*(KiB|MiB|GiB)$`)
	upToSize    = regexp.MustCompile(`^up to\s+([0-9.]+)\s*(KiB|MiB|GiB)$`)
	exactSize   = regexp.MustCompile(`^([0-9]+)$`)
	sha256Value = regexp.MustCompile(`^[0-9a-f]{64}$`)
)

func unit(name string) float64 {
	switch name {
	case "KiB":
		return 1 << 10
	case "MiB":
		return 1 << 20
	}
	return 1 << 30
}

func parseSize(text string) (sizeRule, error) {
	text = strings.TrimSpace(strings.ReplaceAll(text, ",", ""))
	if m := exactSize.FindStringSubmatch(text); m != nil {
		n, err := strconv.ParseInt(m[1], 10, 64)
		return sizeRule{exact: n}, err
	}
	if m := approxSize.FindStringSubmatch(text); m != nil {
		n, err := strconv.ParseFloat(m[1], 64)
		return sizeRule{approx: int64(n * unit(m[2]))}, err
	}
	if m := upToSize.FindStringSubmatch(text); m != nil {
		n, err := strconv.ParseFloat(m[1], 64)
		return sizeRule{upTo: int64(n * unit(m[2]))}, err
	}
	if text == "varies" {
		return sizeRule{varies: true}, nil
	}
	return sizeRule{}, fmt.Errorf("size %q is not bytes, ~N MiB, up to N MiB or varies", text)
}

// check says why a size breaks the rule, or "".
func (rule sizeRule) check(size int64) string {
	switch {
	case rule.exact > 0 && size != rule.exact:
		return fmt.Sprintf("size %d, want exactly %d", size, rule.exact)
	case rule.approx > 0 && (size < rule.approx*9/10 || size > rule.approx*11/10):
		return fmt.Sprintf("size %d, want within 10%% of %d", size, rule.approx)
	case rule.upTo > 0 && (size == 0 || size > rule.upTo):
		return fmt.Sprintf("size %d, want 1 to %d", size, rule.upTo)
	case rule.varies && size == 0:
		return "empty"
	}
	return ""
}

// ParseFixtureTable reads the fixture rows out of bench/fixtures/README.md.
// Rows under a heading containing "Harness extras" are the harness's own.
func ParseFixtureTable(r io.Reader) ([]Fixture, error) {
	var fixtures []Fixture
	extra := false
	scanner := bufio.NewScanner(r)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "#") {
			extra = strings.Contains(line, "Harness extras")
			continue
		}
		m := tableRow.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		fixture := Fixture{Name: m[1], Size: strings.TrimSpace(m[2]), How: strings.TrimSpace(m[4]), Extra: extra}
		if sum := strings.Trim(strings.TrimSpace(m[3]), "`"); sha256Value.MatchString(sum) {
			fixture.SHA256 = sum
		} else if sum != "-" {
			return nil, fmt.Errorf("fixture %s: SHA-256 column %q is neither a digest nor -", fixture.Name, sum)
		}
		if _, err := parseSize(fixture.Size); err != nil {
			return nil, fmt.Errorf("fixture %s: %w", fixture.Name, err)
		}
		fixtures = append(fixtures, fixture)
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	if len(fixtures) == 0 {
		return nil, fmt.Errorf("no fixture rows found")
	}
	return fixtures, nil
}

// extraFixture is a fixture the harness builds itself: the shapes the matrix
// needs that `cargo xtask fixtures` does not make.
type extraFixture struct {
	Name string
	// Tool is "xz" or "7zz".
	Tool string
	// Args run in the fixtures directory; an xz build writes stdout to the
	// fixture, a 7zz build names the archive itself.
	Args []string
	// Filter is the xz filter the build needs, so a missing one is a skip.
	Filter string
}

// bcjExtras are the branch filters xtask does not make a fixture for.
var bcjExtras = []string{"arm", "armthumb", "ppc", "sparc", "ia64", "riscv"}

func extraFixtures() []extraFixture {
	extras := []extraFixture{
		{Name: "p256.none.xz", Tool: "xz", Args: []string{"-T1", "-5", "--check=none", "-c", "p256.bin"}},
		{Name: "p256.st.7z", Tool: "7zz", Args: []string{"a", "-t7z", "-bso0", "-bsp0", "-mx=5", "-m0=lzma2", "-mmt=1", "p256.st.7z", "p256.bin"}},
		{Name: "p256.mt.7z", Tool: "7zz", Args: []string{"a", "-t7z", "-bso0", "-bsp0", "-mx=5", "-m0=lzma2", "-mmt=on", "p256.mt.7z", "p256.bin"}},
	}
	for _, f := range bcjExtras {
		extras = append(extras, extraFixture{
			Name:   "bcj-" + f + ".code.xz",
			Tool:   "xz",
			Args:   []string{"-T1", "-5", XZFilterOption(f), "--lzma2=preset=5", "-c", "codebin.bin"},
			Filter: f,
		})
	}
	return extras
}

// FixtureOptions configures `fixtures`.
type FixtureOptions struct {
	// VerifyOnly checks what is there and builds nothing.
	VerifyOnly bool
	Log        func(format string, args ...any)
}

// manifestName caches digests so a re-verify does not re-hash 5 GiB.
const manifestName = "manifest.json"

// Fixtures builds the missing fixtures (xtask's, then the harness extras)
// and verifies every one against the README table.
func Fixtures(ctx context.Context, paths Paths, tc Toolchain, options FixtureOptions) ([]FixtureStatus, error) {
	logf := options.Log
	if logf == nil {
		logf = func(string, ...any) {}
	}
	readme, err := os.Open(filepath.Join(paths.Fixtures, "README.md"))
	if err != nil {
		readme, err = os.Open(filepath.Join(paths.Repo, "bench", "fixtures", "README.md"))
	}
	if err != nil {
		return nil, err
	}
	table, err := ParseFixtureTable(readme)
	readme.Close()
	if err != nil {
		return nil, fmt.Errorf("bench/fixtures/README.md: %w", err)
	}
	skipped := map[string]string{}
	if !options.VerifyOnly {
		if err := os.MkdirAll(paths.Fixtures, 0o755); err != nil {
			return nil, err
		}
		if err := runXtaskFixtures(ctx, paths, tc, logf); err != nil {
			return nil, err
		}
		for _, extra := range extraFixtures() {
			if reason := buildExtra(ctx, paths, tc, extra, logf); reason != "" {
				skipped[extra.Name] = reason
			}
		}
	}

	cache := map[string]Digest{}
	if data, err := os.ReadFile(filepath.Join(paths.Fixtures, manifestName)); err == nil {
		var digests []Digest
		if json.Unmarshal(data, &digests) == nil {
			for _, d := range digests {
				cache[d.Name] = d
			}
		}
	}
	var statuses []FixtureStatus
	var digests []Digest
	for _, fixture := range table {
		status := FixtureStatus{Fixture: fixture}
		path := filepath.Join(paths.Fixtures, fixture.Name)
		info, err := os.Stat(path)
		switch {
		case err != nil && skipped[fixture.Name] != "":
			status.Status, status.Reason = "skipped", skipped[fixture.Name]
		case err != nil:
			status.Status, status.Reason = "missing", "not generated"
		default:
			digest, ok := cache[fixture.Name]
			if !ok || digest.Size != info.Size() || digest.ModUnixNano != info.ModTime().UnixNano() {
				logf("hashing %s", fixture.Name)
				if digest, err = DigestFile(path); err != nil {
					return nil, err
				}
			}
			digests = append(digests, digest)
			status.Digest = &digest
			rule, _ := parseSize(fixture.Size)
			if reason := rule.check(digest.Size); reason != "" {
				status.Status, status.Reason = "mismatch", reason
			} else if fixture.SHA256 != "" && digest.SHA256 != fixture.SHA256 {
				status.Status, status.Reason = "mismatch", "SHA-256 "+digest.SHA256+", want "+fixture.SHA256
			} else {
				status.Status = "ok"
			}
		}
		statuses = append(statuses, status)
	}
	if data, err := json.MarshalIndent(digests, "", "  "); err == nil {
		_ = os.WriteFile(filepath.Join(paths.Fixtures, manifestName), append(data, '\n'), 0o644)
	}
	return statuses, nil
}

// FixtureDigests reads the manifest `fixtures` wrote, for the runner's
// expected CRCs, hashing any file the manifest does not cover.
func FixtureDigests(paths Paths, names []string) (map[string]Digest, error) {
	cache := map[string]Digest{}
	if data, err := os.ReadFile(filepath.Join(paths.Fixtures, manifestName)); err == nil {
		var digests []Digest
		if json.Unmarshal(data, &digests) == nil {
			for _, d := range digests {
				cache[d.Name] = d
			}
		}
	}
	out := map[string]Digest{}
	for _, name := range names {
		path := filepath.Join(paths.Fixtures, name)
		info, err := os.Stat(path)
		if err != nil {
			continue
		}
		if d, ok := cache[name]; ok && d.Size == info.Size() && d.ModUnixNano == info.ModTime().UnixNano() {
			out[name] = d
			continue
		}
		d, err := DigestFile(path)
		if err != nil {
			return nil, err
		}
		out[name] = d
	}
	return out, nil
}

// runXtaskFixtures runs `cargo xtask fixtures`, which wants `xz`, `7zz` and
// `tar` on PATH by those names. When the 7-Zip the toolchain resolved has
// another name or is not on PATH (the pinned download, or 7za.exe on
// Windows), a shim directory naming it 7zz goes first on PATH.
func runXtaskFixtures(ctx context.Context, paths Paths, tc Toolchain, logf func(string, ...any)) error {
	env := os.Environ()
	var prepend []string
	if tc.SevenZip.Found() {
		if onPath, err := exec.LookPath("7zz"); err != nil || !sameFile(onPath, tc.SevenZip.Path) {
			shim := filepath.Join(paths.Fixtures, ".tools")
			if err := os.MkdirAll(shim, 0o755); err != nil {
				return err
			}
			target := filepath.Join(shim, exe("7zz"))
			_ = os.Remove(target)
			if err := linkOrCopy(tc.SevenZip.Path, target); err != nil {
				return fmt.Errorf("7zz shim: %w", err)
			}
			prepend = append(prepend, shim)
		}
	}
	if tc.XZ.Found() {
		if onPath, err := exec.LookPath("xz"); err != nil || !sameFile(onPath, tc.XZ.Path) {
			prepend = append(prepend, filepath.Dir(tc.XZ.Path))
		}
	}
	if len(prepend) > 0 {
		env = append(env, "PATH="+strings.Join(append(prepend, os.Getenv("PATH")), string(os.PathListSeparator)))
	}
	logf("generating fixtures: cargo xtask fixtures")
	cmd := exec.CommandContext(ctx, "cargo", "xtask", "fixtures")
	cmd.Dir = paths.Repo
	cmd.Env = env
	cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("cargo xtask fixtures: %w", err)
	}
	if paths.Fixtures != filepath.Join(paths.Repo, "bench", "fixtures") {
		logf("note: cargo xtask fixtures always writes to the checkout's bench/fixtures, not %s", paths.Fixtures)
	}
	return nil
}

func sameFile(a, b string) bool {
	ai, err1 := os.Stat(a)
	bi, err2 := os.Stat(b)
	return err1 == nil && err2 == nil && os.SameFile(ai, bi)
}

func linkOrCopy(from, to string) error {
	if runtime.GOOS != "windows" {
		if err := os.Symlink(from, to); err == nil {
			return nil
		}
	}
	in, err := os.Open(from)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(to, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o755)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	return out.Close()
}

// buildExtra makes one harness fixture into a temporary name and renames it
// into place, so an interrupted build never leaves a short file behind. It
// returns why the fixture was skipped, or "".
func buildExtra(ctx context.Context, paths Paths, tc Toolchain, extra extraFixture, logf func(string, ...any)) string {
	final := filepath.Join(paths.Fixtures, extra.Name)
	if _, err := os.Stat(final); err == nil {
		return ""
	}
	tool := tc.XZ
	if extra.Tool == "7zz" {
		tool = tc.SevenZip
	}
	if !tool.Found() {
		return tool.Missing
	}
	if extra.Filter != "" && !contains(tool.Filters, extra.Filter) {
		return fmt.Sprintf("this xz (%s) has no %s filter", tool.Version, extra.Filter)
	}
	logf("fixtures: %s", extra.Name)
	partial := final + ".partial"
	_ = os.Remove(partial)
	args := append([]string(nil), extra.Args...)
	cmd := exec.CommandContext(ctx, tool.Path)
	cmd.Dir = paths.Fixtures
	cmd.Stderr = os.Stderr
	if extra.Tool == "7zz" {
		for i, arg := range args {
			if arg == extra.Name {
				args[i] = filepath.Base(partial)
			}
		}
		// 7-Zip picks the format from -t7z, not the .partial suffix.
		cmd.Args = append(cmd.Args, args...)
		if err := cmd.Run(); err != nil {
			_ = os.Remove(partial)
			return fmt.Sprintf("%s failed: %v", extra.Name, err)
		}
	} else {
		out, err := os.Create(partial)
		if err != nil {
			return err.Error()
		}
		cmd.Args = append(cmd.Args, args...)
		cmd.Stdout = out
		err = cmd.Run()
		if closeErr := out.Close(); err == nil {
			err = closeErr
		}
		if err != nil {
			_ = os.Remove(partial)
			return fmt.Sprintf("%s failed: %v", extra.Name, err)
		}
	}
	if err := os.Rename(partial, final); err != nil {
		return err.Error()
	}
	return ""
}

func contains(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}
