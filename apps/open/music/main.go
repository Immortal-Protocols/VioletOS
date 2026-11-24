// Music: streams the Glass Orchard album from the Rose Music library over
// Wi-Fi and plays it. An ordinary Go program for linux/arm: HTTPS with Go's
// TLS, MP3 decoding in the app, PCM to /dev/violet/audio, and a full
// 720x1600 interface drawn into /dev/violet/screen.
package main

import (
	"fmt"
	"image"
	"os"
	"runtime"
	"runtime/debug"
	"time"
)

func main() {
	fmt.Println("music: starting")
	// A fatal error in one line: the system log is a small ring.
	debug.SetTraceback("none")
	screen, err := OpenScreen()
	if err != nil {
		fmt.Println("music: no screen:", err)
		os.Exit(1)
	}
	touches, err := Events()
	if err != nil {
		fmt.Println("music: no events:", err)
		os.Exit(1)
	}
	faces := LoadFaces()
	frame := image.NewRGBA(image.Rect(0, 0, W, H))
	static := image.NewRGBA(image.Rect(0, 0, W, H))
	splash := func(line string) {
		fill(frame, frame.Bounds(), Palette{}.deepOr())
		centered(frame, faces.big, "Rose Music", 760, accentDefault)
		centered(frame, faces.body, line, 820, inkDefault)
		screen.Show(frame, frame.Bounds())
	}
	splash("Connecting to the library…")

	var catalog *Catalog
	for catalog == nil {
		c, err := LoadCatalog()
		if err != nil {
			fmt.Println("music: catalog:", err)
			splash("Waiting for the network…")
			time.Sleep(3 * time.Second)
			continue
		}
		catalog = c
	}
	fmt.Printf("music: %s, %d songs\n", catalog.Album, len(catalog.Tracks))
	player, err := NewPlayer()
	if err != nil {
		splash("No audio: " + err.Error())
		fmt.Println("music: no audio:", err)
		select {}
	}

	covers := map[int]image.Image{}
	cover := func(i int) image.Image {
		if img, ok := covers[i]; ok {
			return img
		}
		img := LoadCover(catalog.Base + "/" + catalog.Tracks[i].Cover)
		covers[i] = img
		return img
	}
	shown := -2
	var pal Palette
	redraw := func(current int) {
		art := cover(max(current, 0))
		pal = PaletteOf(orBlank(art))
		Static(static, faces, catalog, art, current, pal)
		copy(frame.Pix, static.Pix)
		Dynamic(frame, static, faces, catalog, player, pal, "")
		screen.Show(frame, frame.Bounds())
		shown = current
	}
	play := func(i int) {
		i = (i + len(catalog.Tracks)) % len(catalog.Tracks)
		player.Play(catalog, i)
		t := catalog.Tracks[i]
		fmt.Printf("music: playing %d %s (%d BPM, %s)\n", t.Number, t.Title, t.BPM, t.Key)
	}
	redraw(-1)
	// Start the album right away.
	play(0)

	tick := time.NewTicker(100 * time.Millisecond)
	lastLog := time.Now()
	for {
		select {
		case t, ok := <-touches:
			if !ok {
				return
			}
			if t.Kind == Lifecycle && t.X == 1 {
				shown = -2 // back in front: draw everything again
			}
			if t.Kind != TouchDown {
				continue
			}
			control, song := Hit(t.X, t.Y, len(catalog.Tracks))
			current := int(player.track.Load())
			switch control {
			case "home":
				System("home")
			case "play":
				if current < 0 {
					play(0)
				} else {
					player.paused.Store(!player.paused.Load())
				}
			case "prev":
				if current >= 0 && player.Position() > 3 {
					play(current)
				} else {
					play(current - 1)
				}
			case "next":
				play(current + 1)
			case "song":
				play(song)
			}
		case done := <-player.finished:
			if int(player.track.Load()) == done {
				play(done + 1) // the album repeats
			}
		case <-tick.C:
			current := int(player.track.Load())
			if current != shown {
				redraw(current)
				continue
			}
			status, _ := player.failure.Load().(string)
			Dynamic(frame, static, faces, catalog, player, pal, status)
			screen.Show(frame, dynamic)
			if time.Since(lastLog) > 10*time.Second && current >= 0 {
				lastLog = time.Now()
				var m runtime.MemStats
				runtime.ReadMemStats(&m)
				fmt.Printf("music: %s at %s%s (heap %d MiB, from system %d MiB, %d goroutines, gc %d)\n", catalog.Tracks[current].Title,
					clock(player.Position()), map[bool]string{true: " (paused)", false: ""}[player.paused.Load()],
					m.HeapAlloc>>20, m.Sys>>20, runtime.NumGoroutine(), m.NumGC)
			}
		}
	}
}

func orBlank(img image.Image) image.Image {
	if img == nil {
		return image.NewRGBA(image.Rect(0, 0, 8, 8))
	}
	return img
}

var (
	accentDefault = rgbaOf(0xe6, 0x8c, 0xc8)
	inkDefault    = rgbaOf(0xee, 0xea, 0xf8)
)
