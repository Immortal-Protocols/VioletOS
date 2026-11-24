package main

// The VioletOS devices, through ordinary Linux files (runtime/open/abi.rs).

import (
	"encoding/binary"
	"image"
	"os"
	"syscall"
	"unsafe"
)

const (
	W = 720
	H = 1600
)

type Screen struct {
	file *os.File
	pix  []uint32 // the mapped BGRA8888 surface, buffer 0
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
	pix := unsafe.Slice((*uint32)(unsafe.Pointer(&mem[0])), W*H)
	return &Screen{file: f, pix: pix}, nil
}

// Copy a rectangle of an RGBA image into the surface and show those rows.
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
	binary.LittleEndian.PutUint32(rect[0:], 0)
	binary.LittleEndian.PutUint32(rect[4:], uint32(r.Min.Y))
	binary.LittleEndian.PutUint32(rect[8:], W)
	binary.LittleEndian.PutUint32(rect[12:], uint32(r.Dy()))
	s.file.Write(rect[:])
}

type Touch struct {
	Kind, X, Y int
}

const (
	TouchDown = 1
	TouchUp   = 3
	Lifecycle = 7
)

// Events delivers touch and lifecycle events from /dev/violet/events.
func Events() (<-chan Touch, error) {
	f, err := os.Open("/dev/violet/events")
	if err != nil {
		return nil, err
	}
	ch := make(chan Touch, 16)
	go func() {
		var raw [24]byte
		for {
			if _, err := f.Read(raw[:]); err != nil {
				close(ch)
				return
			}
			kind := int(binary.LittleEndian.Uint16(raw[8:]))
			x := int(int32(binary.LittleEndian.Uint32(raw[12:])))
			y := int(int32(binary.LittleEndian.Uint32(raw[16:])))
			v := int(int32(binary.LittleEndian.Uint32(raw[20:])))
			if kind == Lifecycle {
				x = v
			}
			ch <- Touch{kind, x, y}
		}
	}()
	return ch, nil
}

// System sends one line to /dev/violet/sys and returns the answer.
func System(line string) string {
	f, err := os.OpenFile("/dev/violet/sys", os.O_RDWR, 0)
	if err != nil {
		return ""
	}
	defer f.Close()
	f.Write([]byte(line + "\n"))
	buf := make([]byte, 256)
	n, _ := f.Read(buf)
	return string(buf[:n])
}
