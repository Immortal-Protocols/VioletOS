// Home: the main screen. A clock and the weather, the phone's status, and a
// tile for each app. It stays running; "home" in /dev/violet/sys brings it
// back to the front.
package main

import (
	"fmt"
	"image"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	v "violet/lib"
)

var cities = []string{
	"San Francisco", "Oakland", "Los Angeles", "Seattle", "New York",
	"Chicago", "Austin", "Miami", "Denver", "Boston",
	"Honolulu", "Anchorage", "Mexico City", "London", "Paris",
	"Berlin", "Tokyo", "Sydney", "Mumbai", "Cairo",
}

// The city Weather last showed (apps/open/weather keeps it in /data/weather).
func city() string {
	if b, err := os.ReadFile("/data/weather"); err == nil {
		if n, err := strconv.Atoi(strings.TrimSpace(string(b))); err == nil && n >= 0 && n < len(cities) {
			return cities[n]
		}
	}
	return cities[0]
}

type glance struct {
	utc    time.Time // when the answer was sent
	offset int       // seconds east of UTC at the city
	at     time.Time // our monotonic clock when it arrived
	temp   string
	sky    string
	city   string
}

var client = &http.Client{Timeout: 20 * time.Second}

// fetch asks wttr.in for the local time, temperature and sky at a city; the
// HTTP Date header gives the UTC date.
func fetch(name string) (glance, error) {
	u := "https://wttr.in/" + url.PathEscape(strings.ReplaceAll(name, " ", "+")) + "?format=%T|%t|%C&u"
	req, _ := http.NewRequest("GET", u, nil)
	req.Header.Set("User-Agent", "curl/8 (VioletOS Home)")
	resp, err := client.Do(req)
	if err != nil {
		return glance{}, err
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 256))
	if resp.StatusCode != 200 {
		return glance{}, fmt.Errorf("wttr.in: %s", resp.Status)
	}
	f := strings.Split(strings.TrimSpace(string(body)), "|")
	if len(f) < 3 {
		return glance{}, fmt.Errorf("unexpected answer %q", body)
	}
	g := glance{at: time.Now(), temp: strings.TrimPrefix(strings.TrimSpace(f[1]), "+"), sky: strings.TrimSpace(f[2]), city: name}
	g.utc, err = http.ParseTime(resp.Header.Get("Date"))
	if err != nil {
		return g, nil
	}
	// "18:47:32-0800": the offset is the last five characters.
	if t := strings.TrimSpace(f[0]); len(t) >= 5 {
		z := t[len(t)-5:]
		if h, err1 := strconv.Atoi(z[1:3]); err1 == nil {
			if m, err2 := strconv.Atoi(z[3:5]); err2 == nil {
				g.offset = h*3600 + m*60
				if z[0] == '-' {
					g.offset = -g.offset
				}
			}
		}
	}
	return g, nil
}

func (g glance) now() (time.Time, bool) {
	if g.utc.IsZero() {
		return time.Time{}, false
	}
	return g.utc.Add(time.Since(g.at)).In(time.FixedZone("", g.offset)), true
}

func main() {
	screen, err := v.OpenScreen()
	if err != nil {
		fmt.Println("home: no screen:", err)
		os.Exit(1)
	}
	events, err := v.Events()
	if err != nil {
		fmt.Println("home: no events:", err)
		os.Exit(1)
	}
	fc := LoadFonts()
	frame := image.NewRGBA(image.Rect(0, 0, v.W, v.H))
	st := State{Battery: -1, Pressed: -1, Running: map[string]bool{}}
	var g glance
	glances := make(chan glance, 1)
	refresh := func() {
		go func() {
			name := city()
			r, err := fetch(name)
			if err != nil {
				fmt.Println("home: weather:", err)
				r = glance{city: ""}
			}
			glances <- r
		}()
	}
	status := func() {
		f := v.Fields(v.System("status"))
		st.Battery, _ = strconv.Atoi(f["battery"])
		if f["battery"] == "" {
			st.Battery = -1
		}
		st.Volume, _ = strconv.Atoi(f["volume"])
		st.Phones = f["headphones"] == "1"
		st.Running = map[string]bool{}
		for _, p := range strings.Fields(v.System("running")) {
			st.Running[p] = true
		}
	}
	draw := func() {
		st.Now, st.HaveTime = g.now()
		Draw(frame, fc, st)
		screen.Show(frame, frame.Bounds())
	}
	status()
	refresh()
	draw()
	fmt.Println("home: ready")
	tick := time.NewTicker(5 * time.Second)
	lastFetch := time.Now()
	shownMinute := -1
	for {
		select {
		case e, ok := <-events:
			if !ok {
				return
			}
			switch {
			case e.Kind == v.Lifecycle && e.Value == v.FocusGained:
				st.Pressed = -1
				status()
				draw()
				if time.Since(lastFetch) > 2*time.Minute {
					lastFetch = time.Now()
					refresh()
				}
			case e.Kind == v.TouchDown:
				st.Pressed = tileAt(image.Pt(e.X, e.Y))
				if st.Pressed >= 0 {
					draw()
				}
			case e.Kind == v.TouchUp:
				i := st.Pressed
				st.Pressed = -1
				if i >= 0 {
					draw()
					fmt.Println("home: opening", tiles[i].Name)
					if answer := v.System(tiles[i].Command); answer != "ok" {
						fmt.Println("home:", tiles[i].Command, "->", answer)
					}
				}
			}
		case r := <-glances:
			if r.city != "" {
				g = r
				st.Online, st.Temp, st.Sky, st.City = true, r.temp, r.sky, r.city
				fmt.Printf("home: %s %s %s\n", r.city, r.temp, r.sky)
			} else {
				st.Online = false
			}
			draw()
		case <-tick.C:
			if time.Since(lastFetch) > 15*time.Minute || (!st.Online && time.Since(lastFetch) > time.Minute) {
				lastFetch = time.Now()
				refresh()
			}
			now, ok := g.now()
			old := st
			status()
			changed := old.Battery != st.Battery || old.Volume != st.Volume || old.Phones != st.Phones || len(old.Running) != len(st.Running)
			if (ok && now.Minute() != shownMinute) || changed {
				shownMinute = now.Minute()
				draw()
			}
		}
	}
}
