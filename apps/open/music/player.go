package main

// Streams a track over HTTPS, decodes its MP3 in this process and plays
// 48 kHz PCM through /dev/violet/audio. The Audio service only mixes and sets
// the volume: no decoder or URL lives in an OS service.

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"math/cmplx"
	"net/http"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/hajimehoshi/go-mp3"
)

const Library = "https://agent-39868422bcfa413fa7149c56084664db.apps.chariots.sh"

type Track struct {
	Number     int       `json:"number"`
	Title      string    `json:"title"`
	Artist     string    `json:"artist"`
	DurationMs int       `json:"durationMs"`
	BPM        int       `json:"bpm"`
	Key        string    `json:"key"`
	Audio      string    `json:"audio"`
	Cover      string    `json:"cover"`
	Thumb      string    `json:"thumb"`
	Peaks      []float64 `json:"peaks"`
}

type Catalog struct {
	Name   string  `json:"name"`
	Album  string  `json:"album"`
	Base   string  `json:"baseUrl"`
	Tracks []Track `json:"tracks"`
}

var client = &http.Client{Timeout: 0}

// The library serves bounded ranges only: a whole-file or open-ended request
// gets 503. Everything is fetched in pieces of this size.
const piece = 128 << 10

// get fetches bytes from..from+piece-1 of a file.
func get(url string, from int64) (*http.Response, error) {
	req, err := http.NewRequest("GET", url, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", from, from+piece-1))
	// The library sleeps when idle and answers 503 while it wakes.
	for attempt := 0; ; attempt++ {
		resp, err := client.Do(req)
		if err == nil && (resp.StatusCode == 200 || resp.StatusCode == 206 || resp.StatusCode == 416) {
			return resp, nil
		}
		if err == nil {
			resp.Body.Close()
			err = fmt.Errorf("%s: %s", url, resp.Status)
		}
		if attempt == 6 {
			return nil, err
		}
		time.Sleep(time.Duration(attempt+1) * 700 * time.Millisecond)
	}
}

// fetch reads a whole file, piece by piece.
func fetch(url string) ([]byte, error) {
	stop := &atomic.Bool{}
	r := &resumable{url: url, stop: stop, status: &atomic.Value{}, total: -1, giveUp: true}
	defer r.Close()
	return io.ReadAll(r)
}

func LoadCatalog() (*Catalog, error) {
	data, err := fetch(Library + "/catalog.json")
	if err != nil {
		return nil, err
	}
	var c Catalog
	if err := json.Unmarshal(data, &c); err != nil {
		return nil, err
	}
	if c.Base == "" {
		c.Base = Library
	}
	return &c, nil
}

// resumable reads a URL, reopening it from where it stopped if the
// connection drops (Wi-Fi hiccups should not end a song).
type resumable struct {
	url    string
	at     int64
	total  int64 // from Content-Range; -1 until known
	body   io.ReadCloser
	stop   *atomic.Bool
	status *atomic.Value // "Reconnecting…" while the network is away
	giveUp bool          // fail after a few tries instead of waiting forever
	lost   bool          // a request failed and has not succeeded since
}

func (r *resumable) Read(p []byte) (int, error) {
	for tries := 0; ; tries++ {
		if r.stop.Load() {
			return 0, io.EOF
		}
		if r.total >= 0 && r.at >= r.total {
			return 0, io.EOF
		}
		if r.body == nil {
			resp, err := get(r.url, r.at)
			if err == nil && resp.StatusCode == 416 {
				resp.Body.Close()
				return 0, io.EOF
			}
			if err == nil {
				// "bytes a-b/total"; a 200 is the whole file.
				var a, b, total int64
				if _, e := fmt.Sscanf(resp.Header.Get("Content-Range"), "bytes %d-%d/%d", &a, &b, &total); e == nil {
					r.total = total
				} else if resp.StatusCode == 200 && resp.ContentLength >= 0 {
					r.total = r.at + resp.ContentLength
				}
			}
			if err != nil && r.giveUp && tries > 3 {
				return 0, err
			}
			if err != nil {
				// Keep trying while the network comes back; the song continues
				// from the same byte.
				if !r.lost {
					fmt.Println("music: stream interrupted:", short(err))
				}
				r.lost = true
				r.status.Store("Reconnecting…")
				time.Sleep(time.Duration(min(tries+1, 5)) * time.Second)
				continue
			}
			if r.lost {
				fmt.Println("music: stream resumed")
				r.lost = false
			}
			r.status.Store("")
			r.body = resp.Body
		}
		n, err := r.body.Read(p)
		r.at += int64(n)
		if err == io.EOF {
			// The end of this piece; the next Read fetches the following one.
			r.body.Close()
			r.body = nil
			if n > 0 {
				return n, nil
			}
			if r.total < 0 {
				return 0, io.EOF
			}
			continue
		}
		if err != nil {
			r.body.Close()
			r.body = nil
			if n > 0 {
				return n, nil
			}
			continue
		}
		return n, nil
	}
}
func (r *resumable) Close() {
	if r.body != nil {
		r.body.Close()
	}
}

const Bands = 16

type Player struct {
	audio *os.File
	mu    sync.Mutex
	// What is playing; written by the player, read by the UI.
	track    atomic.Int32 // index, -1 when stopped
	frames   atomic.Int64 // frames written for the current track
	paused   atomic.Bool
	failure  atomic.Value // string
	bands    [Bands]atomic.Uint32
	stop     *atomic.Bool
	done     chan struct{}
	finished chan int // the index of a track that played to its end
}

func NewPlayer() (*Player, error) {
	f, err := os.OpenFile("/dev/violet/audio", os.O_WRONLY, 0)
	if err != nil {
		return nil, err
	}
	p := &Player{audio: f, finished: make(chan int, 4)}
	p.track.Store(-1)
	p.failure.Store("")
	return p, nil
}

func (p *Player) Stop() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.stop != nil {
		p.stop.Store(true)
		<-p.done
		p.stop = nil
	}
	p.track.Store(-1)
	p.paused.Store(false)
}

func (p *Player) Play(c *Catalog, index int) {
	p.Stop()
	p.mu.Lock()
	defer p.mu.Unlock()
	stop := &atomic.Bool{}
	done := make(chan struct{})
	p.stop, p.done = stop, done
	p.track.Store(int32(index))
	p.frames.Store(0)
	p.paused.Store(false)
	p.failure.Store("")
	go p.run(c.Base+"/"+c.Tracks[index].Audio, index, stop, done)
}

func (p *Player) run(url string, index int, stop *atomic.Bool, done chan struct{}) {
	defer close(done)
	stream := &resumable{url: url, stop: stop, status: &p.failure, total: -1}
	src := readAhead(stream, stop)
	defer src.Close() // unblocks the download when a song is skipped
	dec, err := mp3.NewDecoder(src)
	if err != nil {
		p.failure.Store("could not start the song: " + short(err))
		return
	}
	buf := make([]byte, 4096) // 1024 stereo frames
	window := make([]float64, 0, 1024)
	for !stop.Load() {
		if p.paused.Load() {
			time.Sleep(20 * time.Millisecond)
			continue
		}
		n, err := io.ReadFull(dec, buf)
		if n > 0 {
			p.analyze(buf[:n], &window)
			for off := 0; off < n && !stop.Load(); {
				w, werr := p.audio.Write(buf[off:n])
				if werr != nil {
					p.failure.Store("audio: " + short(werr))
					return
				}
				off += w
			}
			p.frames.Add(int64(n / 4))
		}
		if err != nil {
			if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
				if !stop.Load() {
					fmt.Printf("music: song ended after %d s (%v; read %d of %d bytes)\n", p.frames.Load()/48000, err, stream.at, stream.total)
					p.finished <- index
				}
			} else {
				p.failure.Store("stream: " + short(err))
			}
			return
		}
	}
}

func short(err error) string {
	s := err.Error()
	if i := strings.LastIndex(s, ": "); i >= 0 && len(s) > 60 {
		s = s[i+2:]
	}
	return s
}

// A 1024-point FFT of the mono mix, folded into 16 log-spaced bands.
func (p *Player) analyze(pcm []byte, window *[]float64) {
	for i := 0; i+3 < len(pcm); i += 4 {
		l := int16(uint16(pcm[i]) | uint16(pcm[i+1])<<8)
		r := int16(uint16(pcm[i+2]) | uint16(pcm[i+3])<<8)
		*window = append(*window, (float64(l)+float64(r))/65536)
		if len(*window) == 1024 {
			p.spectrum(*window)
			*window = (*window)[:0]
		}
	}
}

func (p *Player) spectrum(x []float64) {
	n := len(x)
	a := make([]complex128, n)
	for i, v := range x {
		hann := 0.5 - 0.5*math.Cos(2*math.Pi*float64(i)/float64(n-1))
		a[i] = complex(v*hann, 0)
	}
	fft(a)
	edges := [Bands + 1]float64{}
	for b := 0; b <= Bands; b++ {
		// 40 Hz to 16 kHz, logarithmically.
		edges[b] = 40 * math.Pow(16000.0/40, float64(b)/Bands)
	}
	for b := 0; b < Bands; b++ {
		lo := int(edges[b] * float64(n) / 48000)
		hi := int(edges[b+1] * float64(n) / 48000)
		if hi <= lo {
			hi = lo + 1
		}
		sum := 0.0
		for k := lo; k < hi && k < n/2; k++ {
			sum += cmplx.Abs(a[k])
		}
		// Normalized so a full-scale sine in one band reads 0 dB; show -66 dB to -6 dB.
		amp := sum / float64(hi-lo) / (float64(n) / 4)
		level := 20 * math.Log10(amp+1e-9)
		v := (level + 66) / 60
		v = math.Max(0, math.Min(1, v))
		old := math.Float32frombits(p.bands[b].Load())
		smooth := math.Max(v, float64(old)*0.82)
		p.bands[b].Store(math.Float32bits(float32(smooth)))
	}
}

func fft(a []complex128) {
	n := len(a)
	for i, j := 1, 0; i < n; i++ {
		bit := n >> 1
		for ; j&bit != 0; bit >>= 1 {
			j ^= bit
		}
		j ^= bit
		if i < j {
			a[i], a[j] = a[j], a[i]
		}
	}
	for size := 2; size <= n; size <<= 1 {
		step := cmplx.Exp(complex(0, -2*math.Pi/float64(size)))
		for start := 0; start < n; start += size {
			w := complex(1, 0)
			for k := 0; k < size/2; k++ {
				u, v := a[start+k], a[start+k+size/2]*w
				a[start+k], a[start+k+size/2] = u+v, u-v
				w *= step
			}
		}
	}
}

func (p *Player) Bands() [Bands]float64 {
	var out [Bands]float64
	for i := range out {
		out[i] = float64(math.Float32frombits(p.bands[i].Load()))
		if p.paused.Load() || p.track.Load() < 0 {
			out[i] = 0
		}
	}
	return out
}

// Seconds heard: frames written less what is still queued in Audio's ring.
func (p *Player) Position() float64 {
	s := float64(p.frames.Load())/48000 - 0.3
	return math.Max(0, s)
}

// readAhead keeps downloading up to a minute of the song in the background,
// so the next piece arrives before the decoder needs it.
func readAhead(r *resumable, stop *atomic.Bool) *io.PipeReader {
	pr, pw := io.Pipe()
	chunks := make(chan []byte, 8) // 8 x 128 KiB, about a minute at 128 kbps
	go func() {
		defer close(chunks)
		defer r.Close()
		for !stop.Load() {
			buf := make([]byte, piece)
			n, err := io.ReadFull(r, buf)
			if n > 0 {
				chunks <- buf[:n]
			}
			if err != nil {
				return
			}
		}
	}()
	go func() {
		for c := range chunks {
			if stop.Load() {
				break
			}
			if _, err := pw.Write(c); err != nil {
				break
			}
		}
		pw.Close()
		for range chunks {
		}
	}()
	return pr
}
