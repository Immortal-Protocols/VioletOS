package violet

import (
	"image"
	"image/color"
	"math"

	"golang.org/x/image/font"
	"golang.org/x/image/font/gofont/gobold"
	"golang.org/x/image/font/gofont/gomedium"
	"golang.org/x/image/font/gofont/goregular"
	"golang.org/x/image/font/opentype"
	"golang.org/x/image/math/fixed"
)

// The shared look of the open apps.
var (
	Background = RGB(0x14, 0x0e, 0x1e)
	Panel      = RGB(0x2a, 0x20, 0x3c)
	PanelHi    = RGB(0x3c, 0x2e, 0x55)
	Ink        = RGB(0xee, 0xea, 0xf8)
	Muted      = RGB(0xa8, 0x9c, 0xc4)
	Accent     = RGB(0xe6, 0x8c, 0xc8)
)

func RGB(r, g, b uint8) color.RGBA { return color.RGBA{r, g, b, 255} }

type Faces struct{ Huge, Title, Big, Body, Small font.Face }

// Face loads a TrueType font at a pixel size.
func Face(ttf []byte, size float64) font.Face {
	f, _ := opentype.Parse(ttf)
	ff, _ := opentype.NewFace(f, &opentype.FaceOptions{Size: size, DPI: 72, Hinting: font.HintingFull})
	return ff
}

func LoadFaces() Faces {
	return Faces{
		Huge:  Face(gobold.TTF, 120),
		Title: Face(gobold.TTF, 44),
		Big:   Face(gobold.TTF, 38),
		Body:  Face(goregular.TTF, 30),
		Small: Face(gomedium.TTF, 24),
	}
}

func Fill(img *image.RGBA, r image.Rectangle, c color.RGBA) {
	r = r.Intersect(img.Bounds())
	for y := r.Min.Y; y < r.Max.Y; y++ {
		for x := r.Min.X; x < r.Max.X; x++ {
			i := img.PixOffset(x, y)
			img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = c.R, c.G, c.B, 255
		}
	}
}

// Blend draws c over a pixel with coverage a (0..1).
func Blend(img *image.RGBA, x, y int, c color.RGBA, a float64) {
	if !image.Pt(x, y).In(img.Bounds()) || a <= 0 {
		return
	}
	a = math.Min(a, 1)
	i := img.PixOffset(x, y)
	for k, v := range [3]uint8{c.R, c.G, c.B} {
		img.Pix[i+k] = uint8(float64(img.Pix[i+k])*(1-a) + float64(v)*a)
	}
}

// RoundRect fills a rectangle with anti-aliased rounded corners.
func RoundRect(img *image.RGBA, r image.Rectangle, radius float64, c color.RGBA) {
	ri := int(math.Ceil(radius))
	// The middle is a plain fill; only the four corners need coverage.
	Fill(img, image.Rect(r.Min.X+ri, r.Min.Y, r.Max.X-ri, r.Max.Y), c)
	Fill(img, image.Rect(r.Min.X, r.Min.Y+ri, r.Min.X+ri, r.Max.Y-ri), c)
	Fill(img, image.Rect(r.Max.X-ri, r.Min.Y+ri, r.Max.X, r.Max.Y-ri), c)
	for y := r.Min.Y; y < r.Max.Y; y++ {
		if y >= r.Min.Y+ri && y < r.Max.Y-ri {
			continue
		}
		for x := r.Min.X; x < r.Max.X; x++ {
			if x >= r.Min.X+ri && x < r.Max.X-ri {
				continue
			}
			fx, fy := float64(x)+0.5, float64(y)+0.5
			dx := math.Max(math.Max(float64(r.Min.X)+radius-fx, fx-(float64(r.Max.X)-radius)), 0)
			dy := math.Max(math.Max(float64(r.Min.Y)+radius-fy, fy-(float64(r.Max.Y)-radius)), 0)
			Blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-(math.Hypot(dx, dy)-radius))))
		}
	}
}

// Text draws s with its baseline at y and returns the x where it ended.
func Text(img *image.RGBA, f font.Face, s string, x, y int, c color.RGBA) int {
	d := &font.Drawer{Dst: img, Src: image.NewUniform(c), Face: f, Dot: fixed.P(x, y)}
	d.DrawString(s)
	return d.Dot.X.Round()
}

func TextWidth(f font.Face, s string) int { return font.MeasureString(f, s).Round() }

// Centered draws s centred in [x0, x1).
func Centered(img *image.RGBA, f font.Face, s string, x0, x1, y int, c color.RGBA) {
	Text(img, f, s, x0+(x1-x0-TextWidth(f, s))/2, y, c)
}

// Fit shortens s with an ellipsis until it is at most width pixels wide.
func Fit(f font.Face, s string, width int) string {
	r := []rune(s)
	for TextWidth(f, string(r)) > width && len(r) > 1 {
		r = append(r[:len(r)-2], '…')
	}
	return string(r)
}

// Button draws a rounded button with a centred label.
func Button(img *image.RGBA, f font.Face, r image.Rectangle, label string, bg, ink color.RGBA) {
	RoundRect(img, r, 22, bg)
	m := f.Metrics()
	base := r.Min.Y + (r.Dy()+m.Ascent.Round()-m.Descent.Round())/2
	Centered(img, f, label, r.Min.X, r.Max.X, base, ink)
}

// HomeBox is where every app draws its "‹ Home" button.
var HomeBox = image.Rect(28, 34, 188, 92)

// Header draws the "‹ Home" button and a title.
func Header(img *image.RGBA, fc Faces, title string) {
	Button(img, fc.Small, HomeBox, "‹ Home", Panel, Ink)
	Text(img, fc.Title, title, 40, 180, Ink)
}

// Circle fills an anti-aliased disc.
func Circle(img *image.RGBA, cx, cy, r float64, c color.RGBA) {
	for y := int(cy - r - 1); y <= int(cy+r+1); y++ {
		for x := int(cx - r - 1); x <= int(cx+r+1); x++ {
			d := math.Hypot(float64(x)+0.5-cx, float64(y)+0.5-cy) - r
			Blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

// Ring strokes a circle of radius r with the given width.
func Ring(img *image.RGBA, cx, cy, r, width float64, c color.RGBA) {
	for y := int(cy - r - width); y <= int(cy+r+width); y++ {
		for x := int(cx - r - width); x <= int(cx+r+width); x++ {
			d := math.Abs(math.Hypot(float64(x)+0.5-cx, float64(y)+0.5-cy)-r) - width/2
			Blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

// Arc strokes the part of a ring between two angles (radians, 0 = right,
// counter-clockwise on screen is negative y).
func Arc(img *image.RGBA, cx, cy, r, width, from, to float64, c color.RGBA) {
	for y := int(cy - r - width); y <= int(cy+r+width); y++ {
		for x := int(cx - r - width); x <= int(cx+r+width); x++ {
			fx, fy := float64(x)+0.5-cx, float64(y)+0.5-cy
			a := math.Atan2(-fy, fx)
			if a < from || a > to {
				continue
			}
			d := math.Abs(math.Hypot(fx, fy)-r) - width/2
			Blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

// Line strokes a segment with round ends.
func Line(img *image.RGBA, x0, y0, x1, y1, width float64, c color.RGBA) {
	minX, maxX := math.Min(x0, x1)-width, math.Max(x0, x1)+width
	minY, maxY := math.Min(y0, y1)-width, math.Max(y0, y1)+width
	dx, dy := x1-x0, y1-y0
	l2 := dx*dx + dy*dy
	for y := int(minY); y <= int(maxY); y++ {
		for x := int(minX); x <= int(maxX); x++ {
			px, py := float64(x)+0.5, float64(y)+0.5
			t := 0.0
			if l2 > 0 {
				t = math.Max(0, math.Min(1, ((px-x0)*dx+(py-y0)*dy)/l2))
			}
			d := math.Hypot(px-(x0+t*dx), py-(y0+t*dy)) - width/2
			Blend(img, x, y, c, math.Max(0, math.Min(1, 0.5-d)))
		}
	}
}

// Triangle fills a triangle, anti-aliased by 4x4 supersampling.
func Triangle(img *image.RGBA, p [3][2]float64, c color.RGBA) {
	minX, maxX := math.Min(p[0][0], math.Min(p[1][0], p[2][0])), math.Max(p[0][0], math.Max(p[1][0], p[2][0]))
	minY, maxY := math.Min(p[0][1], math.Min(p[1][1], p[2][1])), math.Max(p[0][1], math.Max(p[1][1], p[2][1]))
	edge := func(a, b [2]float64, x, y float64) float64 { return (b[0]-a[0])*(y-a[1]) - (b[1]-a[1])*(x-a[0]) }
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
			Blend(img, x, y, c, float64(hits)/16)
		}
	}
}

// Gradient fills r with a vertical blend from top to bottom.
func Gradient(img *image.RGBA, r image.Rectangle, top, bottom color.RGBA) {
	r = r.Intersect(img.Bounds())
	for y := r.Min.Y; y < r.Max.Y; y++ {
		t := float64(y-r.Min.Y) / float64(max(r.Dy()-1, 1))
		c := Mix(top, bottom, t)
		for x := r.Min.X; x < r.Max.X; x++ {
			i := img.PixOffset(x, y)
			img.Pix[i], img.Pix[i+1], img.Pix[i+2], img.Pix[i+3] = c.R, c.G, c.B, 255
		}
	}
}

// Mix blends two colours (t = 0 gives a, 1 gives b).
func Mix(a, b color.RGBA, t float64) color.RGBA {
	f := func(x, y uint8) uint8 { return uint8(float64(x)*(1-t) + float64(y)*t + 0.5) }
	return color.RGBA{f(a.R, b.R), f(a.G, b.G), f(a.B, b.B), 255}
}
