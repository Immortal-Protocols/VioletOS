// Fetch web pages with Go's net/http: plain HTTP, then HTTPS with Go's own TLS.
package main

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"regexp"
	"time"
)

func fetch(url string) {
	start := time.Now()
	client := &http.Client{Timeout: 25 * time.Second}
	reply, err := client.Get(url)
	if err != nil {
		fmt.Println("gonet:", url, "failed:", err)
		return
	}
	defer reply.Body.Close()
	body, _ := io.ReadAll(reply.Body)
	title := regexp.MustCompile(`(?s)<title>(.*?)</title>`).FindSubmatch(body)
	name := ""
	if title != nil {
		name = string(title[1])
	}
	fmt.Printf("gonet: %s -> %s, %d bytes, title %q in %v\n", url, reply.Status, len(body), name, time.Since(start).Round(time.Millisecond))
}

func main() {
	addrs, err := net.LookupHost("example.com")
	fmt.Println("gonet: example.com ->", addrs, err)
	fetch("http://example.com/")
	if len(os.Args) < 2 || os.Args[1] != "nohttps" {
		fetch("https://example.com/")
	}
	fmt.Println("gonet done")
}
