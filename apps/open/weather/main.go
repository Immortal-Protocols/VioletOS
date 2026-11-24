// Weather: current conditions for twenty cities, fetched from wttr.in over
// HTTPS by this app (Go's TLS, the system's root certificates). The last
// city chosen is kept in /data/weather.
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

const (
	saved   = "/data/weather"
	gridTop = 800
	rowH    = 66
	rows    = 10
)

var current = image.Rect(0, 200, v.W, 780)

type report struct {
	temp, condition, humidity, wind string
}

var client = &http.Client{Timeout: 20 * time.Second}

func fetch(city string) (report, error) {
	u := "https://wttr.in/" + url.PathEscape(strings.ReplaceAll(city, " ", "+")) + "?format=%t;%C;%h;%w&u"
	u = strings.ReplaceAll(u, "%252B", "+")
	req, _ := http.NewRequest("GET", u, nil)
	req.Header.Set("User-Agent", "curl/8 (VioletOS Weather)")
	resp, err := client.Do(req)
	if err != nil {
		return report{}, err
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 512))
	if resp.StatusCode != 200 {
		return report{}, fmt.Errorf("wttr.in: %s", resp.Status)
	}
	f := strings.Split(strings.TrimSpace(string(body)), ";")
	if len(f) < 2 || f[0] == "" {
		return report{}, fmt.Errorf("unexpected answer %q", body)
	}
	for len(f) < 4 {
		f = append(f, "")
	}
	for i := range f {
		f[i] = strings.Map(func(r rune) rune {
			if r > 0xff {
				return -1 // arrows and symbols the font lacks
			}
			return r
		}, strings.TrimSpace(f[i]))
	}
	return report{strings.TrimPrefix(f[0], "+"), f[1], f[2], f[3]}, nil
}

func cell(i int) image.Rectangle {
	col, row := i/rows, i%rows
	x := 40 + col*330
	y := gridTop + row*rowH
	return image.Rect(x, y, x+310, y+rowH-10)
}

func main() {
	screen, err := v.OpenScreen()
	if err != nil {
		fmt.Println("weather: no screen:", err)
		os.Exit(1)
	}
	events, err := v.Events()
	if err != nil {
		fmt.Println("weather: no events:", err)
		os.Exit(1)
	}
	fc := v.LoadFaces()
	frame := image.NewRGBA(image.Rect(0, 0, v.W, v.H))

	city := -1
	if b, err := os.ReadFile(saved); err == nil {
		if n, err := strconv.Atoi(strings.TrimSpace(string(b))); err == nil && n >= 0 && n < len(cities) {
			city = n
		}
	}
	var shown report
	status := "Tap a city for live weather"
	loading := false
	results := make(chan struct {
		city int
		r    report
		err  error
	}, 4)
	start := func(i int) {
		city, loading, shown = i, true, report{}
		status = "Fetching…"
		go func() {
			r, err := fetch(cities[i])
			results <- struct {
				city int
				r    report
				err  error
			}{i, r, err}
		}()
	}

	drawCurrent := func() {
		v.Fill(frame, current, v.Background)
		if city < 0 {
			v.Text(frame, fc.Big, "Choose a city", 40, 300, v.Ink)
		} else {
			v.Text(frame, fc.Big, cities[city], 40, 300, v.Ink)
			if shown.temp != "" {
				v.Text(frame, fc.Huge, shown.temp, 40, 470, v.Accent)
				v.Text(frame, fc.Body, v.Fit(fc.Body, shown.condition, v.W-80), 40, 560, v.Ink)
				if shown.humidity != "" {
					v.Text(frame, fc.Small, "Humidity "+shown.humidity+"   Wind "+shown.wind, 40, 620, v.Muted)
				}
			}
		}
		v.Text(frame, fc.Small, status, 40, 720, v.Muted)
	}
	drawGrid := func() {
		for i, name := range cities {
			bg, ink := v.Panel, v.Ink
			if i == city {
				bg, ink = v.Accent, v.Background
			}
			r := cell(i)
			v.Fill(frame, r.Inset(-4), v.Background)
			v.RoundRect(frame, r, 16, bg)
			v.Text(frame, fc.Small, name, r.Min.X+20, r.Min.Y+38, ink)
		}
	}
	draw := func() {
		v.Fill(frame, frame.Bounds(), v.Background)
		v.Header(frame, fc, "Weather")
		drawCurrent()
		drawGrid()
		screen.Show(frame, frame.Bounds())
	}
	if city >= 0 {
		start(city)
	}
	draw()
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
			if p.In(v.HomeBox) {
				v.Home()
				continue
			}
			for i := range cities {
				if p.In(cell(i)) && !loading {
					start(i)
					os.WriteFile(saved, []byte(strconv.Itoa(i)+"\n"), 0o644)
					draw()
				}
			}
		case r := <-results:
			if r.city != city {
				continue
			}
			loading = false
			if r.err != nil {
				fmt.Println("weather:", cities[r.city], r.err)
				status = "Could not fetch – tap to retry"
			} else {
				shown = r.r
				status = "Live from wttr.in"
				fmt.Printf("weather: %s %s %s\n", cities[r.city], shown.temp, shown.condition)
			}
			drawCurrent()
			screen.Show(frame, current)
		}
	}
}
