package main

import (
	"bytes"
	"fmt"
	"image"
	"image/color"
	"image/jpeg"
	"math"

	xdraw "golang.org/x/image/draw"
	"golang.org/x/image/font"
	"golang.org/x/image/font/gofont/gobold"
	"golang.org/x/image/font/gofont/gomedium"
	"golang.org/x/image/font/gofont/goregular"
	"golang.org/x/image/font/opentype"
	"golang.org/x/image/math/fixed"
)

// Layout, in screen pixels.
var (
	homeBox  = image.Rect(28, 34, 188, 92)
	coverBox = image.Rect(110, 150, 610, 650)
	waveBox  = image.Rect(50, 880, 670, 940)
	prevAt   = image.Pt(180, 1080)
	playAt   = image.Pt(360, 1080)
	nextAt   = image.Pt(540, 1080)
	specBox  = image.Rect(50, 1170, 670, 1240)
	listTop  = 1262
	rowH     = 33
	dynamic  = image.Rect(0, 860, W, 1250)
)

type Faces struct{ title, big, body, small, tiny font.Face }

func face(ttf []byte, size float64) font.Face {
	f, _ := opentype.Parse(ttf)
	ff, _ := opentype.NewFace(f, &opentype.FaceOptions{Size: size, DPI: 72, Hinting: font.HintingFull})
	return ff
}
func LoadFaces() Faces {
	return Faces{
		title: face(gobold.TTF, 44),
		big:   face(gobold.TTF, 38),
		body:  face(goregular.TTF, 28),
		small: face(gomedium.TTF, 23),
		tiny:  face(gomedium.TTF, 20),
	}
}

type Palette struct{ deep, mid, accent, glow color.RGBA }

func rgba(c color.Color) color.RGBA {
	r, g, b, _ := c.RGBA()
	return color.RGBA{uint8(r >> 8), uint8(g >> 8), uint8(b >> 8), 255}
}
func mix(a, b color.RGBA, t float64) color.RGBA {
	f := func(x, y uint8) uint8 { return uint8(float64(x)*(1-t) + float64(y)*t) }
	return color.RGBA{f(a.R, b.R), f(a.G, b.G), f(a.B, b.B), 255}
}

// Colours from the cover: the average for the background and the most vivid
// light colour for accents.
func PaletteOf(img image.Image) Palette {
	b := img.Bounds()
	var sr, sg, sb, n float64
	best, bestScore := color.RGBA{230, 120, 170, 255}, -1.0
	for y := b.Min.Y; y < b.Max.Y; y += 8 {
		for x := b.Min.X; x < b.Max.X; x += 8 {
			c := rgba(img.At(x, y))
			r, g, bl := float64(c.R), float64(c.G), float64(c.B)
			sr, sg, sb, n = sr+r, sg+g, sb+bl, n+1
			mx, mn := math.Max(r, math.Max(g, bl)), math.Min(r, math.Min(g, bl))
			score := (mx - mn) * (0.4 + mx/255)
			if mx > 110 && score > bestScore {
				best, bestScore = c, score
			}
		}
	}
	avg := color.RGBA{uint8(sr / n), uint8(sg / n), uint8(sb / n), 255}
	black := color.RGBA{8, 8, 14, 255}
	return Palette{
		deep:   mix(avg, black, 0.78),
		mid:    mix(avg, black, 0.55),
		accent: mix(best, color.RGBA{255, 255, 255, 255}, 0.15),
		glow:   mix(best, black, 0.35),
	}
}

func fill(img *image.RGBA, r image.Rectangle, c color.RGBA) {
	r = r.Intersect(img.Bounds())
	for y := r.Min.Y; y < r.Max.Y; y++ {
		for x := r.Min.X; x < r.Max.X; x++ {
			i := img.PixOffset(x, y)
			img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = c.R, c.G, c.B, 255
		}
	}
}

// blend draws c over the pixel with coverage a (0..1).
func blend(img *image.RGBA, x, y int, c color.RGBA, a float64) {
	if !image.Pt(x, y).In(img.Bounds()) || a <= 0 {
		return
	}
	if a > 1 {
		a = 1
	}
	i := img.PixOffset(x, y)
	for k, v := range [3]uint8{c.R, c.G, c.B} {
		img.Pix[i+k] = uint8(float64(img.Pix[i+k])*(1-a) + float64(v)*a)
	}
}

// A rounded rectangle with anti-aliased corners.
func roundRect(img *image.RGBA, r image.Rectangle, radius float64, c color.RGBA, alpha float64) {
	for y := r.Min.Y; y < r.Max.Y; y++ {
		for x := r.Min.X; x < r.Max.X; x++ {
			fx, fy := float64(x)+0.5, float64(y)+0.5
			dx := math.Max(math.Max(float64(r.Min.X)+radius-fx, fx-(float64(r.Max.X)-radius)), 0)
			dy := math.Max(math.Max(float64(r.Min.Y)+radius-fy, fy-(float64(r.Max.Y)-radius)), 0)
			d := math.Hypot(dx, dy) - radius
			blend(img, x, y, c, alpha*math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

func circle(img *image.RGBA, at image.Point, radius float64, c color.RGBA) {
	for y := at.Y - int(radius) - 1; y <= at.Y+int(radius)+1; y++ {
		for x := at.X - int(radius) - 1; x <= at.X+int(radius)+1; x++ {
			d := math.Hypot(float64(x-at.X)+0.5, float64(y-at.Y)+0.5) - radius
			blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

// A filled triangle, anti-aliased by 4x4 supersampling.
func triangle(img *image.RGBA, p [3][2]float64, c color.RGBA) {
	minX, maxX := math.Min(p[0][0], math.Min(p[1][0], p[2][0])), math.Max(p[0][0], math.Max(p[1][0], p[2][0]))
	minY, maxY := math.Min(p[0][1], math.Min(p[1][1], p[2][1])), math.Max(p[0][1], math.Max(p[1][1], p[2][1]))
	edge := func(a, b [2]float64, x, y float64) float64 {
		return (b[0]-a[0])*(y-a[1]) - (b[1]-a[1])*(x-a[0])
	}
	for y := int(minY) - 1; y <= int(maxY)+1; y++ {
		for x := int(minX) - 1; x <= int(maxX)+1; x++ {
			hits := 0
			for sy := 0; sy < 4; sy++ {
				for sx := 0; sx < 4; sx++ {
					fx, fy := float64(x)+(float64(sx)+0.5)/4, float64(y)+(float64(sy)+0.5)/4
					e0, e1, e2 := edge(p[0], p[1], fx, fy), edge(p[1], p[2], fx, fy), edge(p[2], p[0], fx, fy)
					if (e0 >= 0 && e1 >= 0 && e2 >= 0) || (e0 <= 0 && e1 <= 0 && e2 <= 0) {
						hits++
					}
				}
			}
			blend(img, x, y, c, float64(hits)/16)
		}
	}
}

func text(img *image.RGBA, f font.Face, s string, x, y int, c color.RGBA) int {
	d := &font.Drawer{Dst: img, Src: image.NewUniform(c), Face: f, Dot: fixed.P(x, y)}
	d.DrawString(s)
	return d.Dot.X.Round()
}
func textWidth(f font.Face, s string) int {
	return font.MeasureString(f, s).Round()
}
func centered(img *image.RGBA, f font.Face, s string, y int, c color.RGBA) {
	text(img, f, s, (W-textWidth(f, s))/2, y, c)
}
func fit(f font.Face, s string, width int) string {
	for textWidth(f, s) > width && len(s) > 1 {
		s = s[:len(s)-2] + "…"
	}
	return s
}

func clock(sec float64) string {
	s := int(sec)
	return fmt.Sprintf("%d:%02d", s/60, s%60)
}

func LoadCover(url string) image.Image {
	data, err := fetch(url)
	if err != nil {
		return nil
	}
	img, err := jpeg.Decode(bytes.NewReader(data))
	if err != nil {
		return nil
	}
	// Keep the cover at the size it is drawn, not 1024x1024.
	small := image.NewRGBA(image.Rect(0, 0, coverBox.Dx(), coverBox.Dy()))
	xdraw.CatmullRom.Scale(small, small.Bounds(), img, img.Bounds(), xdraw.Src, nil)
	return small
}

// Static draws everything that changes only with the track.
func Static(img *image.RGBA, fc Faces, c *Catalog, cover image.Image, current int, pal Palette) {
	// Background: the cover, blurred by scaling it to a few pixels and back,
	// darkened towards the bottom.
	if cover != nil {
		tiny := image.NewRGBA(image.Rect(0, 0, 9, 20))
		xdraw.ApproxBiLinear.Scale(tiny, tiny.Bounds(), cover, cover.Bounds(), xdraw.Src, nil)
		xdraw.BiLinear.Scale(img, img.Bounds(), tiny, tiny.Bounds(), xdraw.Src, nil)
	} else {
		fill(img, img.Bounds(), pal.mid)
	}
	for y := 0; y < H; y++ {
		t := 0.45 + 0.5*float64(y)/H
		for x := 0; x < W; x++ {
			blend(img, x, y, pal.deep, t)
		}
	}
	roundRect(img, homeBox, 29, color.RGBA{255, 255, 255, 255}, 0.13)
	text(img, fc.small, "‹  Home", homeBox.Min.X+28, homeBox.Min.Y+38, color.RGBA{245, 240, 250, 255})
	label := c.Name + "  ·  " + c.Album
	text(img, fc.tiny, label, W-36-textWidth(fc.tiny, label), 70, pal.accent)
	if cover != nil {
		// Shadow, then the cover with rounded corners.
		for k := 24; k > 0; k -= 4 {
			roundRect(img, coverBox.Inset(-k).Add(image.Pt(0, 14)), 34+float64(k), color.RGBA{0, 0, 0, 255}, 0.05)
		}
		art := image.NewRGBA(image.Rect(0, 0, coverBox.Dx(), coverBox.Dy()))
		xdraw.CatmullRom.Scale(art, art.Bounds(), cover, cover.Bounds(), xdraw.Src, nil)
		mask := image.NewRGBA(art.Bounds())
		roundRect(mask, mask.Bounds(), 30, color.RGBA{255, 255, 255, 255}, 1)
		for y := 0; y < art.Bounds().Dy(); y++ {
			for x := 0; x < art.Bounds().Dx(); x++ {
				a := float64(mask.Pix[mask.PixOffset(x, y)]) / 255
				i := art.PixOffset(x, y)
				blend(img, coverBox.Min.X+x, coverBox.Min.Y+y, color.RGBA{art.Pix[i], art.Pix[i+1], art.Pix[i+2], 255}, a)
			}
		}
	} else {
		roundRect(img, coverBox, 30, pal.glow, 1)
	}
	if current >= 0 {
		t := c.Tracks[current]
		centered(img, fc.title, fit(fc.title, t.Title, 640), 728, color.RGBA{255, 255, 255, 255})
		centered(img, fc.body, t.Artist, 774, color.RGBA{225, 220, 235, 255})
		centered(img, fc.small, fmt.Sprintf("%d BPM   ·   %s", t.BPM, t.Key), 816, pal.accent)
	} else {
		centered(img, fc.title, c.Album, 728, color.RGBA{255, 255, 255, 255})
		centered(img, fc.body, "Tap a song to play", 774, color.RGBA{225, 220, 235, 255})
	}
	// The album, one row per song.
	for i, t := range c.Tracks {
		y := listTop + i*rowH
		ink := color.RGBA{232, 228, 240, 255}
		if i == current {
			roundRect(img, image.Rect(36, y-2, W-36, y+rowH-2), 12, pal.accent, 0.22)
			ink = color.RGBA{255, 255, 255, 255}
		}
		text(img, fc.tiny, fmt.Sprintf("%02d", t.Number), 54, y+24, pal.accent)
		text(img, fc.tiny, fit(fc.tiny, t.Title, 380), 104, y+24, ink)
		meta := fmt.Sprintf("%d BPM  %s", t.BPM, clock(float64(t.DurationMs)/1000))
		text(img, fc.tiny, meta, W-54-textWidth(fc.tiny, meta), y+24, color.RGBA{185, 178, 200, 255})
	}
}

// Dynamic draws the seek bar, times, controls and spectrum over the static layer.
func Dynamic(img, static *image.RGBA, fc Faces, c *Catalog, p *Player, pal Palette, status string) {
	copy(img.Pix[img.PixOffset(0, dynamic.Min.Y):img.PixOffset(0, dynamic.Max.Y)],
		static.Pix[static.PixOffset(0, dynamic.Min.Y):static.PixOffset(0, dynamic.Max.Y)])
	current := int(p.track.Load())
	dim := color.RGBA{255, 255, 255, 255}
	if current >= 0 {
		t := c.Tracks[current]
		length := float64(t.DurationMs) / 1000
		pos := math.Min(p.Position(), length)
		played := pos / length
		peaks := t.Peaks
		bars := 62
		w := float64(waveBox.Dx()) / float64(bars)
		for b := 0; b < bars; b++ {
			v := 0.25
			if len(peaks) > 0 {
				v = peaks[b*len(peaks)/bars]
			}
			h := math.Max(4, v*float64(waveBox.Dy())*1.6)
			h = math.Min(h, float64(waveBox.Dy()))
			x0 := float64(waveBox.Min.X) + float64(b)*w
			mid := float64(waveBox.Min.Y+waveBox.Max.Y) / 2
			r := image.Rect(int(x0+1.5), int(mid-h/2), int(x0+w-1.5), int(mid+h/2))
			if float64(b)/float64(bars) < played {
				roundRect(img, r, 3, pal.accent, 1)
			} else {
				roundRect(img, r, 3, dim, 0.22)
			}
		}
		text(img, fc.tiny, clock(pos), waveBox.Min.X, waveBox.Max.Y+38, color.RGBA{225, 220, 235, 255})
		end := clock(length)
		text(img, fc.tiny, end, waveBox.Max.X-textWidth(fc.tiny, end), waveBox.Max.Y+38, color.RGBA{225, 220, 235, 255})
	}
	// Previous, play or pause, next.
	white := color.RGBA{255, 255, 255, 255}
	circle(img, playAt, 62, white)
	if current >= 0 && !p.paused.Load() {
		roundRect(img, image.Rect(playAt.X-20, playAt.Y-24, playAt.X-6, playAt.Y+24), 3, pal.deep, 1)
		roundRect(img, image.Rect(playAt.X+6, playAt.Y-24, playAt.X+20, playAt.Y+24), 3, pal.deep, 1)
	} else {
		x, y := float64(playAt.X), float64(playAt.Y)
		triangle(img, [3][2]float64{{x - 16, y - 27}, {x - 16, y + 27}, {x + 28, y}}, pal.deep)
	}
	for _, s := range []struct {
		at  image.Point
		dir float64
	}{{prevAt, -1}, {nextAt, 1}} {
		x, y := float64(s.at.X), float64(s.at.Y)
		triangle(img, [3][2]float64{{x - 18*s.dir, y - 20}, {x - 18*s.dir, y + 20}, {x + 14*s.dir, y}}, white)
		bar := image.Rect(int(x+14*s.dir)-3, int(y)-20, int(x+14*s.dir)+3, int(y)+20)
		if s.dir > 0 {
			bar = bar.Add(image.Pt(2, 0))
		} else {
			bar = bar.Add(image.Pt(-2, 0))
		}
		roundRect(img, bar, 2, white, 1)
	}
	// Spectrum.
	bands := p.Bands()
	bw := float64(specBox.Dx()) / Bands
	for b, v := range bands {
		h := math.Max(6, v*float64(specBox.Dy()))
		x0 := float64(specBox.Min.X) + float64(b)*bw
		r := image.Rect(int(x0+5), specBox.Max.Y-int(h), int(x0+bw-5), specBox.Max.Y)
		roundRect(img, r, 5, mix(pal.accent, white, 0.15*v), 0.55+0.45*v)
	}
	if status != "" {
		centered(img, fc.tiny, status, 1166, color.RGBA{255, 200, 200, 255})
	}
}

// Hit tests: which control or song a touch lands on.
func Hit(x, y int, songs int) (control string, song int) {
	p := image.Pt(x, y)
	near := func(at image.Point, r int) bool { return math.Hypot(float64(x-at.X), float64(y-at.Y)) < float64(r) }
	switch {
	case p.In(homeBox.Inset(-12)):
		return "home", -1
	case near(playAt, 80):
		return "play", -1
	case near(prevAt, 70):
		return "prev", -1
	case near(nextAt, 70):
		return "next", -1
	case p.In(waveBox.Inset(-20)):
		return "seek", -1
	case y >= listTop && y < listTop+songs*rowH:
		return "song", (y - listTop) / rowH
	}
	return "", -1
}

func rgbaOf(r, g, b uint8) color.RGBA { return color.RGBA{r, g, b, 255} }
func (Palette) deepOr() color.RGBA    { return color.RGBA{20, 14, 30, 255} }
