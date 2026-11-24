#!/bin/python3
"""Benign: fetches the forecast from its own API."""
import urllib.request

with urllib.request.urlopen("http://api.weather.test/forecast", timeout=20) as r:
    body = r.read().decode()
print("weather:", r.status, body.strip()[:60], flush=True)
print("weather done", flush=True)
