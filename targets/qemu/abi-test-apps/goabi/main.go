// Go under the Open Runtime (make test-system), without GODEBUG=asyncpreemptoff=1:
// the runtime sets it for every app, so this program first execve's itself with
// it removed. Then goroutines spinning in tight loops must not starve a timer
// (asynchronous preemption by SIGURG to a thread running user code), os/signal
// gets a signal, a nil dereference becomes a recoverable panic (SIGSEGV with a
// handler), and os/exec runs a child.
package main

import (
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"strings"
	"syscall"
	"time"
)

func main() {
	if strings.Contains(os.Getenv("GODEBUG"), "asyncpreemptoff=1") {
		var env []string
		for _, e := range os.Environ() {
			if !strings.HasPrefix(e, "GODEBUG=") {
				env = append(env, e)
			}
		}
		err := syscall.Exec("/apps/goabi", os.Args, env)
		fmt.Println("goabi: execve failed:", err)
		os.Exit(1)
	}
	fmt.Println("goabi: running without GODEBUG", os.Getenv("GODEBUG") == "")
	runtime.GOMAXPROCS(1)
	for i := 0; i < 2; i++ {
		go func() {
			for {
			}
		}()
	}
	start := time.Now()
	for round := 0; round < 20; round++ {
		time.Sleep(20 * time.Millisecond)
	}
	fmt.Println("goabi: preempted busy loops", time.Since(start) < 20*time.Second)

	c := make(chan os.Signal, 1)
	signal.Notify(c, syscall.SIGUSR1)
	syscall.Kill(os.Getpid(), syscall.SIGUSR1)
	select {
	case s := <-c:
		fmt.Println("goabi: os/signal received", s)
	case <-time.After(10 * time.Second):
		fmt.Println("goabi: os/signal received nothing")
	}

	func() {
		defer func() { fmt.Println("goabi: recovered:", recover()) }()
		var p *int
		*p = 1
	}()

	out, err := exec.Command("/apps/abichild", "go").Output()
	fmt.Printf("goabi: child said %q err %v\n", strings.TrimSpace(string(out)), err)
	fmt.Println("goabi done")
	os.Exit(0)
}
