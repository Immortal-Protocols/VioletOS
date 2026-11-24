package main

import "testing"

func TestFetch(t *testing.T) {
	r, err := fetch("San Francisco")
	if err != nil {
		t.Skip("wttr.in unavailable:", err)
	}
	if r.temp == "" || r.condition == "" {
		t.Fatalf("incomplete report %+v", r)
	}
	t.Logf("%+v", r)
}
