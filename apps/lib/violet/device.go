// Package violet is a small library for VioletOS open apps written in Go:
// the /dev/violet devices (runtime/open/abi.rs) as ordinary Linux files, and
// a few drawing helpers for the 720x1600 screen.
package violet

import (
	"encoding/binary"
	"image"
	"os"
	"strings"
	"syscall"
	"unsafe"
)

const (
	W = 720
	H = 1600
)

type Screen struct {
	file *os.File
	pix  []uint32 // the mapped BGRA8888 surface
}

func OpenScreen() (*Screen, error) {
	f, err := os.OpenFile("/dev/violet/screen", os.O_RDWR, 0)
	if err != nil {
		return nil, err
	}
	var info [6]uint32
	if _, _, e := syscall.Syscall(syscall.SYS_IOCTL, f.Fd(), 0x5601, uintptr(unsafe.Pointer(&info[0]))); e != 0 {
		return nil, e
	}
	mem, err := syscall.Mmap(int(f.Fd()), 0, int(info[5]), syscall.PROT_READ|syscall.PROT_WRITE, syscall.MAP_SHARED)
	if err != nil {
		return nil, err
	}
	return &Screen{file: f, pix: unsafe.Slice((*uint32)(unsafe.Pointer(&mem[0])), W*H)}, nil
}

// Show copies a rectangle of img to the screen and presents those rows.
func (s *Screen) Show(img *image.RGBA, r image.Rectangle) {
	r = r.Intersect(image.Rect(0, 0, W, H))
	if r.Empty() {
		return
	}
	for y := r.Min.Y; y < r.Max.Y; y++ {
		src := img.Pix[img.PixOffset(r.Min.X, y):]
		dst := s.pix[y*W+r.Min.X : y*W+r.Max.X]
		for x := range dst {
			p := src[4*x : 4*x+4 : 4*x+4]
			dst[x] = 0xff000000 | uint32(p[0])<<16 | uint32(p[1])<<8 | uint32(p[2])
		}
	}
	var rect [24]byte
	binary.LittleEndian.PutUint32(rect[4:], uint32(r.Min.Y))
	binary.LittleEndian.PutUint32(rect[8:], W)
	binary.LittleEndian.PutUint32(rect[12:], uint32(r.Dy()))
	s.file.Write(rect[:])
}

// Event kinds from /dev/violet/events.
const (
	TouchDown = 1
	TouchMove = 2
	TouchUp   = 3
	KeyDown   = 4
	KeyUp     = 5
	Jack      = 6
	Lifecycle = 7
)

// Key codes and lifecycle values.
const (
	KeyVolumeUp   = 1
	KeyVolumeDown = 2
	FocusGained   = 1
	FocusLost     = 2
	Quit          = 3
)

type Event struct {
	Kind, Code, X, Y, Value int
}

// Events delivers events from /dev/violet/events on a channel.
func Events() (<-chan Event, error) {
	f, err := os.Open("/dev/violet/events")
	if err != nil {
		return nil, err
	}
	ch := make(chan Event, 16)
	go func() {
		var raw [24]byte
		for {
			if _, err := f.Read(raw[:]); err != nil {
				close(ch)
				return
			}
			ch <- Event{
				Kind:  int(binary.LittleEndian.Uint16(raw[8:])),
				Code:  int(binary.LittleEndian.Uint16(raw[10:])),
				X:     int(int32(binary.LittleEndian.Uint32(raw[12:]))),
				Y:     int(int32(binary.LittleEndian.Uint32(raw[16:]))),
				Value: int(int32(binary.LittleEndian.Uint32(raw[20:]))),
			}
		}
	}()
	return ch, nil
}

// System sends one command to /dev/violet/sys and returns the answer line.
func System(line string) string {
	f, err := os.OpenFile("/dev/violet/sys", os.O_RDWR, 0)
	if err != nil {
		return ""
	}
	defer f.Close()
	f.Write([]byte(line + "\n"))
	buf := make([]byte, 256)
	n, _ := f.Read(buf)
	return strings.TrimSpace(string(buf[:n]))
}

// Fields parses a "key=value key=value" answer from System.
func Fields(answer string) map[string]string {
	out := map[string]string{}
	for _, w := range strings.Fields(answer) {
		if k, v, ok := strings.Cut(w, "="); ok {
			out[k] = v
		}
	}
	return out
}

// Home returns to the Home screen.
func Home() { System("home") }

// Log writes a line to the system log (the USB diagnostics log on the phone).
func Log(line string) { System("log " + line) }
