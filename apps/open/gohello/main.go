// A standard Go program built with GOOS=linux GOARCH=arm GOARM=7 and no
// VioletOS code: goroutines, channels, the garbage collector, timers and files
// all run on the Open Runtime's Linux system calls.
package main

import (
	"fmt"
	"os"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"
)

func main() {
	fmt.Println("hello from go on violet,", runtime.Version(), runtime.GOOS+"/"+runtime.GOARCH)

	results := make(chan int, 8)
	var wg sync.WaitGroup
	for i := 1; i <= 8; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			sum := 0
			for k := 1; k <= n*1000; k++ {
				sum += k
			}
			results <- sum
		}(i)
	}
	wg.Wait()
	close(results)
	total := 0
	for r := range results {
		total += r
	}
	fmt.Println("goroutines summed:", total)

	// Allocate enough to make the collector run several times.
	var keep [][]byte
	for i := 0; i < 200; i++ {
		b := make([]byte, 256<<10)
		b[0] = byte(i)
		if i%20 == 0 {
			keep = append(keep, b)
		}
	}
	runtime.GC()
	var m runtime.MemStats
	runtime.ReadMemStats(&m)
	fmt.Println("gc cycles at least 1:", m.NumGC >= 1, "kept", len(keep))

	start := time.Now()
	timer := time.After(30 * time.Millisecond)
	tick := time.NewTicker(5 * time.Millisecond)
	ticks := 0
loop:
	for {
		select {
		case <-tick.C:
			ticks++
		case <-timer:
			break loop
		}
	}
	tick.Stop()
	fmt.Println("timer fired after at least 30ms:", time.Since(start) >= 30*time.Millisecond, "ticks>0:", ticks > 0)

	if err := os.MkdirAll("/tmp/go", 0o755); err != nil {
		fmt.Println("mkdir failed:", err)
	}
	if err := os.WriteFile("/tmp/go/note.txt", []byte("written by go"), 0o644); err != nil {
		fmt.Println("write failed:", err)
	}
	data, err := os.ReadFile("/tmp/go/note.txt")
	fmt.Println("file round trip:", string(data), err)
	entries, _ := os.ReadDir("/apps")
	var names []string
	for _, e := range entries {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	fmt.Println("apps:", strings.Join(names, ","))
	fmt.Println("go done")
}
