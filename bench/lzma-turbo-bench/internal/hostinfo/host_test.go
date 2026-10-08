package hostinfo

import (
	"context"
	"runtime"
	"testing"
)

func TestCollectDescribesThisMachine(t *testing.T) {
	host := Collect(context.Background(), "")
	if host.OS != runtime.GOOS || host.Architecture != runtime.GOARCH || host.CPUCount < 1 {
		t.Fatalf("host %+v", host)
	}
	if host.Label == "" || host.CPU == "" || host.Tier == "" || host.ISASource == "" {
		t.Fatalf("host %+v", host)
	}
	if runtime.GOARCH == "arm64" && host.Tier == "baseline" {
		t.Fatalf("an arm64 host without NEON: %+v", host)
	}
}

func TestTier(t *testing.T) {
	cases := map[string][]string{
		"avx512":   {"avx2", "avx512bw", "avx512f"},
		"avx2":     {"avx2", "sse4.2"},
		"sve2":     {"neon", "sve", "sve2"},
		"neon":     {"neon"},
		"baseline": nil,
	}
	for want, isa := range cases {
		if got := tier(isa); got != want {
			t.Errorf("tier(%v) = %s, want %s", isa, got, want)
		}
	}
}

func TestParseLoadAverage(t *testing.T) {
	for text, want := range map[string]float64{"0.52 0.40 0.31 1/123 4567\n": 0.52, "{ 10.90 14.30 15.36 }": 10.90} {
		if got, ok := parseLoadAverage(text); !ok || got != want {
			t.Errorf("parseLoadAverage(%q) = %v, %v", text, got, ok)
		}
	}
	if _, ok := parseLoadAverage(""); ok {
		t.Error("empty text parsed")
	}
}
