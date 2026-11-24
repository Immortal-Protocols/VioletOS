// Sounds: the shared volume and output, the built-in sounds, and a few
// seconds recorded from the microphone to play back. The state comes from
// /dev/violet/sys "audio", so the volume buttons and the headphone jack
// show up here as they change.
package main

import (
	"fmt"
	"image"
	"os"
	"strconv"
	"time"

	v "violet/lib"
)

var (
	minusBox  = image.Rect(40, 470, 340, 590)
	plusBox   = image.Rect(380, 470, 680, 590)
	pads      = []pad{{"Chime", "sound chime"}, {"Alert", "sound alert"}, {"Tone", "sound tone 440 1500"}, {"Stop", "sound stop"}}
	recordBox = image.Rect(40, 1150, 340, 1270)
	playBox   = image.Rect(380, 1150, 680, 1270)
	live      = image.Rect(0, 200, v.W, 470)
	micLive   = image.Rect(0, 1040, v.W, 1140)
	status    = image.Rect(0, 640, v.W, 700)
)

type pad struct{ label, command string }

func padBox(i int) image.Rectangle {
	x := 40 + (i%2)*340
	y := 720 + (i/2)*150
	return image.Rect(x, y, x+300, y+120)
}

func num(m map[string]string, k string) int {
	n, _ := strconv.Atoi(m[k])
	return n
}

func main() {
	screen, err := v.OpenScreen()
	if err != nil {
		fmt.Println("sounds: no screen:", err)
		os.Exit(1)
	}
	events, err := v.Events()
	if err != nil {
		fmt.Println("sounds: no events:", err)
		os.Exit(1)
	}
	fc := v.LoadFaces()
	frame := image.NewRGBA(image.Rect(0, 0, v.W, v.H))
	state := v.Fields(v.System("audio"))
	hasMic := func() bool { return state["mic"] == "1" }

	drawLive := func() {
		v.Fill(frame, live, v.Background)
		if state["volume"] == "" {
			v.Text(frame, fc.Big, "No audio service", 40, 300, v.Ink)
			return
		}
		level, top := num(state, "volume"), num(state, "max")
		if top == 0 {
			top = 15
		}
		v.Text(frame, fc.Huge, strconv.Itoa(level), 40, 340, v.Accent)
		v.Text(frame, fc.Small, "VOLUME", 44, 380, v.Muted)
		var output string
		switch {
		case state["headphones"] == "1" && state["headphone_out"] == "1":
			output = "Headphones"
		case state["headphones"] == "1":
			output = "Headphones – no output"
		case state["speaker"] == "1":
			output = "Speaker"
		default:
			output = "Speaker – no output"
		}
		v.Text(frame, fc.Body, output, 300, 300, v.Ink)
		seg := (v.W - 80) / top
		for i := 0; i < top; i++ {
			c := v.Panel
			if i < level {
				c = v.Accent
			}
			v.RoundRect(frame, image.Rect(40+i*seg, 410, 40+i*seg+seg-8, 450), 6, c)
		}
	}
	drawStatus := func() {
		v.Fill(frame, status, v.Background)
		line := map[string]string{"played": "Played", "stopped": "Stopped", "no-output": "No audio output", "ready": "Ready"}[state["outcome"]]
		if p := state["playing"]; p != "" && p != "none" {
			line = "Playing " + p
		}
		v.Text(frame, fc.Small, line, 40, 680, v.Muted)
	}
	drawMic := func() {
		v.Fill(frame, micLive, v.Background)
		if !hasMic() {
			v.Text(frame, fc.Small, "No microphone", 40, 1090, v.Muted)
			return
		}
		ms := num(state, "recording_ms")
		label := fmt.Sprintf("Microphone  %.1f s", float64(ms)/1000)
		if ms == 0 && state["mic_mode"] != "record" {
			label = "Microphone"
		}
		v.Text(frame, fc.Body, label, 40, 1090, v.Ink)
		if state["mic_mode"] != "off" {
			lvl := num(state, "mic_level")
			for i := 0; i < 10; i++ {
				c := v.Panel
				if i*10 < lvl {
					c = v.Accent
				}
				v.RoundRect(frame, image.Rect(40+i*64, 1110, 40+i*64+56, 1134), 6, c)
			}
		}
	}
	drawMicButtons := func() {
		if !hasMic() {
			return
		}
		rec := "Record"
		if state["mic_mode"] == "record" {
			rec = "Finish"
		}
		v.Fill(frame, recordBox, v.Background)
		v.Button(frame, fc.Big, recordBox, rec, v.Accent, v.Background)
		v.Button(frame, fc.Big, playBox, "Play", v.Panel, v.Ink)
	}
	draw := func() {
		v.Fill(frame, frame.Bounds(), v.Background)
		v.Header(frame, fc, "Sounds")
		drawLive()
		v.Button(frame, fc.Title, minusBox, "–", v.Panel, v.Ink)
		v.Button(frame, fc.Title, plusBox, "+", v.Panel, v.Ink)
		drawStatus()
		for i, p := range pads {
			v.Button(frame, fc.Big, padBox(i), p.label, v.Panel, v.Ink)
		}
		drawMic()
		drawMicButtons()
		screen.Show(frame, frame.Bounds())
	}
	draw()
	tick := time.NewTicker(150 * time.Millisecond)
	for {
		select {
		case e, ok := <-events:
			if !ok {
				return
			}
			if e.Kind == v.Lifecycle && e.Value == v.FocusGained {
				draw()
			}
			if e.Kind != v.TouchDown {
				continue
			}
			p := image.Pt(e.X, e.Y)
			switch {
			case p.In(v.HomeBox):
				v.Home()
			case p.In(minusBox), p.In(plusBox):
				dir := "down"
				if p.In(plusBox) {
					dir = "up"
				}
				v.System("volume " + dir)
				v.System("sound click") // let the new level be heard
			case hasMic() && p.In(recordBox):
				if state["mic_mode"] == "record" {
					v.System("mic off")
				} else {
					v.System("mic record")
				}
			case hasMic() && p.In(playBox):
				v.System("sound recording")
			default:
				for i, pd := range pads {
					if p.In(padBox(i)) {
						fmt.Println("sounds:", pd.command, "->", v.System(pd.command))
					}
				}
			}
		case <-tick.C:
		}
		next := v.Fields(v.System("audio"))
		changed := len(next) != len(state)
		for k, val := range next {
			if state[k] != val {
				changed = true
			}
		}
		if changed {
			micModeChanged := next["mic_mode"] != state["mic_mode"] || next["mic"] != state["mic"]
			state = next
			drawLive()
			drawStatus()
			drawMic()
			screen.Show(frame, live)
			screen.Show(frame, status)
			screen.Show(frame, micLive)
			if micModeChanged {
				drawMicButtons()
				screen.Show(frame, image.Rect(0, recordBox.Min.Y, v.W, recordBox.Max.Y))
			}
		}
	}
}
