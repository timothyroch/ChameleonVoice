from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel
from typing import Optional, List
import numpy as np
import wave
import io

try:
    from TTS.api import TTS
except Exception as e:
    raise RuntimeError(f"Could not import Coqui TTS in this venv: {e}")

app = FastAPI(title="Mini XTTS v2 Server")

# Load XTTS v2 once at startup 
try:
    tts = TTS(
        model_name="tts_models/multilingual/multi-dataset/xtts_v2",
        progress_bar=False,
        gpu=False,   
    )
    SAMPLE_RATE = getattr(tts, "output_sample_rate", 24000)  
except Exception as e:
    raise RuntimeError(f"Failed to load XTTS v2 model: {e}")

class TTSRequest(BaseModel):
    text: str
    language: Optional[str] = "en"   
    speaker: Optional[str] = None     
    audio_format: Optional[str] = "wav"  

def clean_and_chunk(text: str, max_len: int = 220) -> List[str]:
    # light cleanup + safe chunking to avoid long inputs causing 500s
    s = "".join(ch for ch in text if ch == "\n" or not ord(ch) < 32).strip()
    # split on typical sentence punctuation
    parts = []
    start = 0
    for seg in s.replace("？","?").replace("！","!").replace("。",".").split("."):
        seg = seg.strip()
        if not seg:
            continue
        if len(seg) <= max_len:
            parts.append(seg)
        else:
            while len(seg) > max_len:
                parts.append(seg[:max_len])
                seg = seg[max_len:]
            if seg:
                parts.append(seg)
    if not parts and s:
        parts = [s]
    return parts

def to_wav_bytes(samples: np.ndarray, sample_rate: int) -> bytes:
    # ensure float32 in [-1, 1]
    if samples.dtype != np.float32:
        samples = samples.astype(np.float32)
    samples = np.clip(samples, -1.0, 1.0)
    pcm16 = (samples * 32767.0).astype(np.int16)

    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sample_rate)
        wf.writeframes(pcm16.tobytes())
    return buf.getvalue()

@app.post("/tts")
def synthesize(req: TTSRequest):
    text = (req.text or "").strip()
    if not text:
        raise HTTPException(status_code=400, detail="text is required")

    lang = (req.language or "en").strip()
    speaker = req.speaker  # can be None; XTTS v2 has internal defaults

    chunks = clean_and_chunk(text)
    # Synthesize each chunk and simple-concatenate waveform
    waves = []
    try:
        for ch in chunks:
            wav = tts.tts(text=ch, speaker=speaker, language=lang)  # returns numpy array (float32)
            waves.append(np.asarray(wav, dtype=np.float32))
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"TTS synthesis failed: {e}")

    if not waves:
        raise HTTPException(status_code=500, detail="No audio generated")

    full = np.concatenate(waves)
    audio = to_wav_bytes(full, SAMPLE_RATE)
    return Response(content=audio, media_type="audio/wav")

@app.get("/health")
def health():
    return {"ok": True, "sr": SAMPLE_RATE}
