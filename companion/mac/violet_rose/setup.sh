#!/usr/bin/env bash
# Rose's Python environment for Violet Agent, from the copy in companion/mac/roseos:
#   companion/mac/violet_rose/setup.sh                 build/rose-env and Rose's voice models
#   companion/mac/violet_rose/setup.sh --local-brain   also Qwen for Rose's local brain (~20 GB)
# Models go to the Hugging Face cache (~/.cache/huggingface), shared with RoseOS.
# Rose's conversation uses Codex (Sign in with ChatGPT: `codex login`) and Node (brew install node).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
rose="$here/../roseos/prototypes/voice-phone-demo"
env="${ROSE_ENV:-$repo/build/rose-env}"
python="${ROSE_PYTHON:-$(command -v python3.12 || echo /opt/homebrew/bin/python3.12)}"

[[ -x "$env/bin/python" ]] || "$python" -m venv "$env"
"$env/bin/pip" install -q -U pip
"$env/bin/pip" install -q -r "$rose/requirements.txt" cryptography

hf="$env/bin/hf"
"$hf" download mlx-community/silero-vad >/dev/null
"$hf" download mlx-community/smart-turn-v3 >/dev/null
"$hf" download mlx-community/nemotron-3.5-asr-streaming-0.6b >/dev/null
"$hf" download mlx-community/parakeet-tdt-0.6b-v3 >/dev/null
"$hf" download gradium/phonon-31m-en --include "*.json" "*.safetensors" "*.jinja" >/dev/null
for id in NbpkqMVS3CJeq2j8 4SZHfMpw-p46Ywgs vjFyp2BcCyTlUmYb NIWIOv5vTi5eRMj8 bXwrkHhURL1VFJdG \
          nGzMHMPAWGaJFHOo LMDIYE0Oa377Qh6Z 40QrMVgAxFiUnP9f d9XCyhjo4JhkHF1d; do
  "$hf" download gradium/phonon-voices "voices/$id.safetensors" >/dev/null
done
if [[ "${1:-}" == "--local-brain" ]]; then
  "$hf" download mlx-community/Qwen3.6-35B-A3B-4bit
fi
command -v node >/dev/null || echo "Install Node for Rose's Codex bridge: brew install node" >&2
echo "Rose's environment: $env (Rose $(head -1 "$here/../roseos/ROSE_VERSION" | cut -c1-8))"
