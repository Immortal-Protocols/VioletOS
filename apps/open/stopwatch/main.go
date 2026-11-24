// Stopwatch: start, pause and reset, to the tenth of a second. The time
// is kept in /data/stopwatch, so a paused watch survives a restart.
package main

import (
	"fmt"
	"image"
	"os"
	"strconv"
	"strings"
	"time"

	v "violet/lib"
)

const saved = "/data/stopwatch"

var (
	startBox = image.Rect(60, 1000, 660, 1130)
	resetBox = image.Rect(60, 1170, 660, 1300)
	digits   = image.Rect(0, 560, v.W, 760)
)

type watch struct {
	base    time.Duration // time on the clock when it last stopped
	since   time.Time     // when it started, while running
	running bool
}

func (w *watch) elapsed() time.Duration {
	if w.running {
		return w.base + time.Since(w.since)
	}
	return w.base
}

func load() time.Duration {
	b, err := os.ReadFile(saved)
	if err != nil {
		return 0
	}
	tenths, err := strconv.Atoi(strings.TrimSpace(string(b)))
	if err != nil {
		return 0
	}
	return time.Duration(tenths) * 100 * time.Millisecond
}

func save(d time.Duration) {
	if err := os.WriteFile(saved, []byte(strconv.Itoa(int(d/(100*time.Millisecond)))+"\n"), 0o644); err != nil {
		fmt.Println("stopwatch: could not save:", err)
	}
}

func format(d time.Duration) string {
	t := int(d / (100 * time.Millisecond))
	s := t / 10
	return fmt.Sprintf("%02d:%02d.%d", s/60%100, s%60, t%10)
}

func main() {
	screen, err := v.OpenScreen()
	if err != nil {
		fmt.Println("stopwatch: no screen:", err)
		os.Exit(1)
	}
	events, err := v.Events()
	if err != nil {
		fmt.Println("stopwatch: no events:", err)
		os.Exit(1)
	}
	fc := v.LoadFaces()
	frame := image.NewRGBA(image.Rect(0, 0, v.W, v.H))
	w := &watch{base: load()}
	fmt.Println("stopwatch: starting at", format(w.base))

	drawDigits := func() {
		v.Fill(frame, digits, v.Background)
		v.Centered(frame, fc.Huge, format(w.elapsed()), 0, v.W, 700, v.Accent)
	}
	draw := func() {
		v.Fill(frame, frame.Bounds(), v.Background)
		v.Header(frame, fc, "Stopwatch")
		drawDigits()
		label := "Start"
		if w.running {
			label = "Pause"
		}
		v.Button(frame, fc.Big, startBox, label, v.Accent, v.Background)
		v.Button(frame, fc.Big, resetBox, "Reset", v.Panel, v.Ink)
		screen.Show(frame, frame.Bounds())
	}
	draw()
	tick := time.NewTicker(50 * time.Millisecond)
	shown := ""
	for {
		select {
		case e, ok := <-events:
			if !ok {
				return
			}
			if e.Kind == v.Lifecycle && e.Value == v.FocusGained {
				draw()
			}
			if e.Kind != v.TouchDown {
				continue
			}
			p := image.Pt(e.X, e.Y)
			switch {
			case p.In(v.HomeBox):
				v.Home()
			case p.In(startBox):
				if w.running {
					w.base, w.running = w.elapsed(), false
					save(w.base)
				} else {
					w.since, w.running = time.Now(), true
				}
				fmt.Println("stopwatch:", map[bool]string{true: "started", false: "paused"}[w.running], "at", format(w.elapsed()))
				draw()
			case p.In(resetBox):
				*w = watch{}
				save(0)
				fmt.Println("stopwatch: reset")
				draw()
			}
		case <-tick.C:
			if s := format(w.elapsed()); s != shown {
				shown = s
				drawDigits()
				screen.Show(frame, digits)
			}
		}
	}
}
