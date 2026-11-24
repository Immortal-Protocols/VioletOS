package main

import (
	"io"
	"sync/atomic"
	"testing"

	"github.com/hajimehoshi/go-mp3"
)

// Needs the network: the catalog and one whole song through the ranged reader.
func TestLibrary(t *testing.T) {
	if testing.Short() {
		t.Skip("network")
	}
	c, err := LoadCatalog()
	if err != nil || len(c.Tracks) != 10 {
		t.Fatalf("catalog: %v", err)
	}
	src := &resumable{url: c.Base + "/" + c.Tracks[1].Audio, stop: &atomic.Bool{}, status: &atomic.Value{}, total: -1, giveUp: true}
	dec, err := mp3.NewDecoder(src)
	if err != nil {
		t.Fatal(err)
	}
	n, err := io.Copy(io.Discard, dec)
	seconds := float64(n) / 4 / float64(dec.SampleRate())
	t.Logf("%s: %d Hz, %.1f s decoded, %d bytes fetched", c.Tracks[1].Title, dec.SampleRate(), seconds, src.at)
	if err != nil || seconds < float64(c.Tracks[1].DurationMs)/1000-2 {
		t.Fatalf("decoded %.1f s: %v", seconds, err)
	}
}
