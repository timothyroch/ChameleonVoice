# ChameleonVoice – Real-Time Speech Translation & TTS

ChameleonVoice is a local-first real-time speech translation and voice cloning system built in Rust, showcasing Whisper.cpp ASR, LibreTranslate, and Coqui XTTS v2.

> This project showcases the skills I developed during my **Summer 2025 internship at Ingenisoft**.
> I built this system to **mirror the technologies and engineering patterns** I worked with:

* **Multi-threaded Rust services** for audio streaming & Whisper.cpp inference
* **Event-driven architecture** with concurrent ASR job execution and SSE output
* **AI-first design** integrating speech translation
* **Local-first pipeline**: Whisper.cpp

---

## Features

* **Live microphone capture** via `ffmpeg`
* **Speech-to-Text (ASR)** with [Whisper.cpp](https://github.com/ggerganov/whisper.cpp)
* **Optional translation** via external translation API
* **Voice synthesis (TTS)** with [Coqui XTTS v2](https://github.com/coqui-ai/TTS)
* **Multi-threaded ASR** with in-order transcript collector
* **Popup dashboard app** (Rust + Actix + WebView) for real-time monitoring

---

## Tech Stack

* **Rust** (Actix-web, tokio, SSE, multi-threaded concurrency)
* **Whisper.cpp** for efficient local ASR
* **Coqui XTTS v2** for neural text-to-speech
* **FastAPI (Python)** for the XTTS wrapper service
* **HTML/CSS/JS** frontend dashboard

---

## Installation

### 1. Clone this repo

```bash
git clone https://github.com/timothyroch/ChameleonVoice.git
cd ChameleonVoice
```

### 2. Install dependencies

* **Rust** (latest stable)
* **ffmpeg** (for microphone capture)
* **Whisper.cpp** (download and build the binary + model file, e.g. `ggml-base.en.bin`)
* **Python 3.10+** with [Coqui TTS](https://github.com/coqui-ai/TTS)

Example for Python:

```bash
pip install TTS fastapi uvicorn requests numpy libretranslate
```

### 3. Configure environment

Copy `.env.example` → `.env` and adjust settings:

```env
PORT=8080
FFMPEG=ffmpeg
FFMPEG_FORMAT=pulse       # or dshow / avfoundation depending on OS
FFMPEG_DEVICE=default
SAMPLE_RATE=16000
CHUNK_MS=1200
WHISPER_MODEL=./models/ggml-base.en.bin
WHISPER_THREADS=4

TRANSLATE_BASE=http://127.0.0.1:5000
COQUI_BASE=http://127.0.0.1:8020
COQUI_VOICE=English
COQUI_SPEAKER_WAV=./test.wav
```

### 4. Prepare a reference voice sample

Coqui XTTS requires a **speaker reference `.wav` file** so it can mimic your voice.

* Record yourself speaking naturally for **10–15 seconds**.
* Save the file as `test.wav` (or any name you like).
* Place it somewhere accessible on your system.
* Update the environment variable `COQUI_SPEAKER_WAV` in your `.env` file to point to this file. For example:

```
COQUI_SPEAKER_WAV=C:/your path to/ChameleonVoice/test.wav
```

Without this file, XTTS will not be able to generate personalized speech.


### 5. Run services

* Start **XTTS server**:

```bash
uvicorn xtts_server:app --host 127.0.0.1 --port 8020
```

* Start **libretranslate**:

```bash
libretranslate --host 127.0.0.1 --port 5000
```

* Start **Rust service**:

```bash
cargo run
```

> The Rust server automatically opens a **popup window dashboard** for interaction.

---

## Usage

### Live pipeline

1. Speak into your microphone.
2. Watch as transcripts, translations, and synthesized audio play in real time.

### Test XTTS directly (PowerShell example)

```powershell
$body = @{
  text         = "Bonjour, je viens de Krypton"
  language     = "fr"
  speaker_wav  = "C:\ path to \ChameleonVoice\test.wav"
  audio_format = "wav"
} | ConvertTo-Json

Invoke-RestMethod `
  -Uri "http://127.0.0.1:8020/tts" `
  -Method Post `
  -ContentType "application/json" `
  -Body $body `
  -OutFile out.wav
```

---

## Why this project?

This project is a **skills showcase**.
It reflects what I learned during my **Ingenisoft internship**:

* Designing **AI-first audio intelligence pipelines**
* Building **multi-threaded inference systems in Rust**
* Integrating **local inference engines** with modern web UIs

---

## Future Improvements

* GPU acceleration for Whisper & XTTS
* Better VAD (voice activity detection)
* More natural chunked translation/segmentation
* **Docker support** for easy deployment (planned)
