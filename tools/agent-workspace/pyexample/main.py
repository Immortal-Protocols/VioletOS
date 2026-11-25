#!/bin/python3
"""Tally: a counter that keeps its count. The working Python example for the agent."""
import violet
from violet import ACCENT, BACKGROUND, BLACK, INK, MUTED, PANEL

count = violet.load("tally", 0)
s = violet.Screen()
PLUS = (60, 900, 600, 220)
RESET = (60, 1180, 600, 140)


def draw():
    s.fill(BACKGROUND)
    s.header("Tally")
    s.rect(60, 300, 600, 480, PANEL, radius=32)
    s.text_center(60, 360, 600, "Count", size=40, color=MUTED, bg=PANEL)
    s.text_center(60, 470, 600, str(count), size=120, bold=True, bg=PANEL)
    s.button(*PLUS, "+1", bg=ACCENT, ink=BLACK, size=64)
    s.button(*RESET, "Reset", size=40)
    s.show()


draw()
for e in violet.events():
    if e.focus:
        draw()
    elif e.down and e.inside(*PLUS):
        count += 1
        violet.save("tally", count)
        violet.beep(880, 60)
        draw()
    elif e.down and e.inside(*RESET):
        count = 0
        violet.save("tally", count)
        draw()
