package main

import (
	"fmt"
	"image"
	"image/color"
	"math"
	"time"

	"golang.org/x/image/font"
	"golang.org/x/image/font/gofont/gobold"
	"golang.org/x/image/font/gofont/gomedium"
	"golang.org/x/image/font/gofont/goregular"

	v "violet/lib"
)

// What Home shows; everything comes from /dev/violet/sys and wttr.in.
type State struct {
	Now      time.Time // local time, when known
	HaveTime bool
	Online   bool
	Battery  int // percent, -1 unknown
	Volume   int
	Phones   bool
	Temp     string
	Sky      string
	City     string
	Running  map[string]bool
	Pressed  int // the tile under a finger, -1 for none
}

type Tile struct {
	Name, Title string
	Command     string // what a tap writes to /dev/violet/sys
	Tint        color.RGBA
	Icon        func(img *image.RGBA, cx, cy float64, ink color.RGBA)
}

var tiles = []Tile{
	{"music", "Music", "open /apps/music", v.RGB(0xe6, 0x5c, 0x9a), iconMusic},
	{"weather", "Weather", "open /apps/weather", v.RGB(0x3d, 0x9b, 0xe9), iconWeather},
	{"stopwatch", "Stopwatch", "open /apps/stopwatch", v.RGB(0xf2, 0x9e, 0x38), iconStopwatch},
	{"sounds", "Sounds", "open /apps/sounds", v.RGB(0x45, 0xc4, 0x8f), iconSounds},
	{"programs", "Programs", "apps", v.RGB(0x8f, 0x7a, 0xe6), iconPrograms},
	{"system", "System", "system", v.RGB(0x7d, 0x86, 0x99), iconSystem},
}

const (
	tileW, tileH = 196, 250
	gridTop      = 740
	gap          = 26
)

func tileRect(i int) image.Rectangle {
	col, row := i%3, i/3
	x := (v.W-3*tileW-2*gap)/2 + col*(tileW+gap)
	y := gridTop + row*(tileH+gap)
	return image.Rect(x, y, x+tileW, y+tileH)
}

func tileAt(p image.Point) int {
	for i := range tiles {
		if p.In(tileRect(i)) {
			return i
		}
	}
	return -1
}

var (
	top    = v.RGB(0x2a, 0x17, 0x40)
	bottom = v.RGB(0x0e, 0x0b, 0x16)
	dim    = v.RGB(0xb8, 0xae, 0xd2)
)

// Draw paints the whole Home screen.
func Draw(img *image.RGBA, fc Fonts, s State) {
	v.Gradient(img, img.Bounds(), top, bottom)
	drawStatus(img, fc, s)

	// Clock and date, or the name when the time is not known yet.
	if s.HaveTime {
		h, m := s.Now.Hour()%12, s.Now.Minute()
		if h == 0 {
			h = 12
		}
		clock := fmt.Sprintf("%d:%02d", h, m)
		end := v.Text(img, fc.Clock, clock, 56, 360, v.Ink)
		v.Text(img, fc.Body, map[bool]string{true: "PM", false: "AM"}[s.Now.Hour() >= 12], end+14, 360, dim)
		v.Text(img, fc.Body, s.Now.Format("Monday, January 2"), 62, 430, dim)
	} else {
		v.Text(img, fc.Clock, "Violet", 56, 360, v.Ink)
		line := "Starting…"
		if !s.Online {
			line = "Offline – Wi-Fi is in System"
		}
		v.Text(img, fc.Body, line, 62, 430, dim)
	}

	// Weather glance.
	card := image.Rect(40, 500, v.W-40, 640)
	v.RoundRect(img, card, 30, v.RGB(0x24, 0x1b, 0x36))
	iconWeather(img, 112, 570, v.Ink)
	if s.Temp != "" {
		end := v.Text(img, fc.Big, s.Temp, 186, 588, v.Ink)
		x := max(end+36, 370)
		v.Text(img, fc.Label, v.Fit(fc.Label, s.Sky, card.Max.X-30-x), x, 560, v.Ink)
		v.Text(img, fc.Small, v.Fit(fc.Small, s.City, card.Max.X-30-x), x, 604, dim)
	} else {
		v.Text(img, fc.Small, "Weather appears here when online", 186, 580, dim)
	}

	for i, t := range tiles {
		drawTile(img, fc, i, t, s.Running["/apps/"+t.Name], s.Pressed == i)
	}

	// The hint at the bottom.
	v.Centered(img, fc.Tiny, "Hold the top edge in any app to come back here", 0, v.W, 1530, v.RGB(0x6c, 0x63, 0x84))
}

func drawTile(img *image.RGBA, fc Fonts, i int, t Tile, running, pressed bool) {
	r := tileRect(i)
	bg := v.RGB(0x24, 0x1b, 0x36)
	if pressed {
		bg = v.RGB(0x3a, 0x2c, 0x55)
	}
	v.RoundRect(img, r, 38, bg)
	cx, cy := float64(r.Min.X+r.Dx()/2), float64(r.Min.Y+102)
	// The icon sits on a disc in the app's colour, lighter at the top.
	for k := 0; k < 8; k++ {
		v.Circle(img, cx, cy-float64(k)*0.6, 66-float64(k)*0.4, v.Mix(t.Tint, v.RGB(255, 255, 255), float64(k)*0.03))
	}
	t.Icon(img, cx, cy, v.RGB(0xff, 0xff, 0xff))
	v.Centered(img, fc.Label, t.Title, r.Min.X, r.Max.X, r.Min.Y+212, v.Ink)
	if running {
		v.Circle(img, cx, float64(r.Max.Y-14), 5, t.Tint)
	}
}

func drawStatus(img *image.RGBA, fc Fonts, s State) {
	y := 78
	v.Text(img, fc.Small, "VioletOS", 56, y, dim)
	x := v.W - 56
	// Battery: an outline filled to the charge.
	if s.Battery >= 0 {
		label := fmt.Sprintf("%d%%", s.Battery)
		x -= v.TextWidth(fc.Small, label)
		v.Text(img, fc.Small, label, x, y, v.Ink)
		x -= 66
		body := image.Rect(x, y-24, x+50, y-2)
		v.RoundRect(img, body, 6, v.Ink)
		v.RoundRect(img, body.Inset(3), 4, bottom)
		fillTo := body.Inset(5)
		fillTo.Max.X = fillTo.Min.X + fillTo.Dx()*min(max(s.Battery, 0), 100)/100
		c := v.Ink
		if s.Battery <= 15 {
			c = v.RGB(0xf0, 0x60, 0x60)
		}
		v.Fill(img, fillTo, c)
		v.Fill(img, image.Rect(x+50, y-17, x+54, y-9), v.Ink)
		x -= 30
	}
	// Wi-Fi: three arcs, dim when offline.
	wc := v.Ink
	if !s.Online {
		wc = v.RGB(0x5a, 0x52, 0x6e)
	}
	cx, cy := float64(x-22), float64(y-2)
	for k, r := range []float64{8, 18, 28} {
		if k == 0 {
			v.Circle(img, cx, cy-3, 4, wc)
			continue
		}
		v.Arc(img, cx, cy-3, r, 5, math.Pi/4, 3*math.Pi/4, wc)
	}
	x -= 80
	// Volume (or headphones).
	label := fmt.Sprintf("%d", s.Volume)
	x -= v.TextWidth(fc.Small, label)
	v.Text(img, fc.Small, label, x, y, v.Ink)
	if s.Phones {
		v.Arc(img, float64(x-26), float64(y-8), 14, 4, 0, math.Pi, v.Ink)
		v.RoundRect(img, image.Rect(x-44, y-12, x-36, y), 3, v.Ink)
		v.RoundRect(img, image.Rect(x-16, y-12, x-8, y), 3, v.Ink)
	} else {
		speaker(img, float64(x-34), float64(y-20), 0.75, v.Ink)
	}
}

// ---- Icons, drawn around (cx, cy) in about a 90 px box. ----

func iconMusic(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	v.Circle(img, cx-16, cy+20, 14, ink)
	v.Circle(img, cx+20, cy+12, 14, ink)
	v.Line(img, cx-4, cy+20, cx-4, cy-26, 7, ink)
	v.Line(img, cx+32, cy+12, cx+32, cy-34, 7, ink)
	v.Line(img, cx-4, cy-26, cx+32, cy-34, 12, ink)
}

func iconWeather(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	sun := v.RGB(0xff, 0xd2, 0x5c)
	v.Circle(img, cx+12, cy-12, 20, sun)
	for k := 0; k < 8; k++ {
		a := float64(k) * math.Pi / 4
		v.Line(img, cx+12+math.Cos(a)*28, cy-12+math.Sin(a)*28, cx+12+math.Cos(a)*36, cy-12+math.Sin(a)*36, 5, sun)
	}
	v.Circle(img, cx-14, cy+12, 18, ink)
	v.Circle(img, cx+8, cy+4, 22, ink)
	v.Circle(img, cx+28, cy+16, 14, ink)
	v.RoundRect(img, image.Rect(int(cx-32), int(cy+12), int(cx+42), int(cy+30)), 9, ink)
}

func iconStopwatch(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	v.Ring(img, cx, cy+6, 30, 8, ink)
	v.Line(img, cx, cy-30, cx, cy-36, 10, ink)
	v.Line(img, cx-10, cy-40, cx+10, cy-40, 8, ink)
	v.Line(img, cx+26, cy-22, cx+32, cy-28, 7, ink)
	v.Line(img, cx, cy+6, cx+12, cy-10, 6, ink)
	v.Circle(img, cx, cy+6, 5, ink)
}

func speaker(img *image.RGBA, x, y, scale float64, ink color.RGBA) {
	s := scale
	v.RoundRect(img, image.Rect(int(x), int(y+8*s), int(x+12*s), int(y+24*s)), 2, ink)
	v.Triangle(img, [3][2]float64{{x + 8*s, y + 8*s}, {x + 26*s, y - 4*s}, {x + 8*s, y + 16*s}}, ink)
	v.Triangle(img, [3][2]float64{{x + 8*s, y + 24*s}, {x + 26*s, y + 36*s}, {x + 8*s, y + 16*s}}, ink)
	v.Fill(img, image.Rect(int(x+8*s), int(y+8*s), int(x+27*s), int(y+25*s)), ink)
	v.Triangle(img, [3][2]float64{{x + 26*s, y - 4*s}, {x + 27*s, y + 36*s}, {x + 20*s, y + 16*s}}, ink)
}

func iconSounds(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	speaker(img, cx-38, cy-16, 1.1, ink)
	v.Arc(img, cx-6, cy+2, 18, 6, -math.Pi/3, math.Pi/3, ink)
	v.Arc(img, cx-6, cy+2, 32, 6, -math.Pi/3, math.Pi/3, ink)
}

func iconPrograms(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	for k := 0; k < 4; k++ {
		x := cx - 30 + float64(k%2)*34
		y := cy - 30 + float64(k/2)*34
		v.RoundRect(img, image.Rect(int(x), int(y), int(x+26), int(y+26)), 7, ink)
	}
}

func iconSystem(img *image.RGBA, cx, cy float64, ink color.RGBA) {
	for k := 0; k < 8; k++ {
		a := float64(k) * math.Pi / 4
		v.Line(img, cx+math.Cos(a)*22, cy+math.Sin(a)*22, cx+math.Cos(a)*34, cy+math.Sin(a)*34, 13, ink)
	}
	v.Ring(img, cx, cy, 22, 14, ink)
	v.Circle(img, cx, cy, 9, ink)
}

// Fonts for Home.
type Fonts struct{ Clock, Big, Body, Label, Small, Tiny font.Face }

func LoadFonts() Fonts {
	return Fonts{
		Clock: v.Face(gobold.TTF, 168),
		Big:   v.Face(gobold.TTF, 46),
		Body:  v.Face(goregular.TTF, 32),
		Label: v.Face(gomedium.TTF, 28),
		Small: v.Face(gomedium.TTF, 26),
		Tiny:  v.Face(goregular.TTF, 21),
	}
}
