package main

import (
	"image"
	"image/png"
	"os"
	"testing"
	"time"

	v "violet/lib"
)

// go test -run Preview writes Home to $HOME_PREVIEW (a PNG) for a look on the Mac.
func TestPreview(t *testing.T) {
	out := os.Getenv("HOME_PREVIEW")
	if out == "" {
		t.Skip("set HOME_PREVIEW")
	}
	img := image.NewRGBA(image.Rect(0, 0, v.W, v.H))
	st := State{Now: time.Date(2026, 11, 23, 18, 47, 0, 0, time.UTC), HaveTime: true, Online: true, Battery: 82, Volume: 8,
		Temp: "54°F", Sky: "Partly cloudy", City: "Oakland", Running: map[string]bool{"/apps/music": true}, Pressed: -1}
	Draw(img, LoadFonts(), st)
	f, _ := os.Create(out)
	defer f.Close()
	png.Encode(f, img)
}

func TestFetch(t *testing.T) {
	g, err := fetch("Oakland")
	if err != nil {
		t.Skip("offline:", err)
	}
	now, ok := g.now()
	if !ok || g.temp == "" {
		t.Fatalf("incomplete glance %+v", g)
	}
	t.Logf("%s %s %s at %s", g.city, g.temp, g.sky, now.Format("Mon Jan 2 3:04 PM"))
}
