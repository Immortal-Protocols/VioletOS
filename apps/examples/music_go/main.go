// RFC sketch: PR #13's Music app as a standard Go binary.
//
// Build with upstream Go (no VioletOS SDK or fork):
//   GOOS=linux GOARCH=arm GOARM=7 go build -o /apps/music/music .
package main

import (
	"encoding/binary"
	"io"
	"net/http"
	"os"
	"syscall"
)

const (
	width  = 720
	height = 1600
)

type presentRect struct {
	X, Y, W, H, BufIdx, Flags uint32
}

type inputEvent struct {
	TimestampUs uint64
	Kind, Code  uint16
	X, Y, Value int32
}

func streamTrack(url string, audio *os.File) {
	// Standard net/http + crypto/tls over Radio's open TCP/DNS sockets.
	resp, err := http.Get(url)
	if err != nil {
		return
	}
	defer resp.Body.Close()

	// Decode MP3 in pure Go (e.g. hajimehoshi/go-mp3) and write 48kHz s16le PCM.
	_, _ = io.Copy(audio, decodeMP3(resp.Body))
}

func decodeMP3(r io.Reader) io.Reader { return r } // RFC placeholder

func main() {
	screen, _ := os.OpenFile("/dev/violet/screen", os.O_RDWR, 0)
	audio, _ := os.OpenFile("/dev/violet/audio", os.O_WRONLY, 0)
	events, _ := os.Open("/dev/violet/events")

	// Zero-copy 720x1600 BGRA8888 pixel slice via standard mmap.
	fb, _ := syscall.Mmap(int(screen.Fd()), 0, width*height*4,
		syscall.PROT_READ|syscall.PROT_WRITE, syscall.MAP_SHARED)
	defer syscall.Munmap(fb)

	// Draw directly into `fb` at native speed, then present changed rows.
	_ = binary.Write(screen, binary.LittleEndian, presentRect{0, 0, width, height, 0, 1})

	// Goroutine streams audio continuously while main goroutine handles touch UI.
	go streamTrack("https://music.example.com/tracks/01.mp3", audio)

	var ev inputEvent
	for binary.Read(events, binary.LittleEndian, &ev) == nil {
		if ev.Kind == 1 { // EVENT_TOUCH_DOWN
			_, _ = ev.X, ev.Y // handle play/pause/seek and present dirty rect
		}
	}
}
