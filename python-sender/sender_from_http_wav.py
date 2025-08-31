import os, io, socket, wave
import numpy as np
import requests

SR_TARGET = 48000
FRAME_MS = 20
SAMPLES_PER_FRAME = SR_TARGET * FRAME_MS // 1000  # 960

def resample_linear(x: np.ndarray, sr_in: int, sr_out: int) -> np.ndarray:
    if sr_in == sr_out:
        return x.astype(np.float32, copy=False)
    ratio = sr_out / sr_in
    n_out = int(round(len(x) * ratio))
    xp = np.linspace(0, len(x) - 1, num=len(x), dtype=np.float64)
    x_new = np.linspace(0, len(x) - 1, num=n_out, dtype=np.float64)
    return np.interp(x_new, xp, x).astype(np.float32)

def chunk_frames_f32(x: np.ndarray, frame_len: int):
    rem = len(x) % frame_len
    if rem:
        x = np.pad(x, (0, frame_len - rem))
    for i in range(0, len(x), frame_len):
        yield x[i:i+frame_len]

def wav_bytes_to_f32_mono(wav_bytes: bytes) -> tuple[np.ndarray, int]:
    with wave.open(io.BytesIO(wav_bytes), "rb") as w:
        sr = w.getframerate()
        ch = w.getnchannels()
        sampwidth = w.getsampwidth()
        n = w.getnframes()
        raw = w.readframes(n)
    if sampwidth == 2:
        x = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    elif sampwidth == 4:
        x = np.frombuffer(raw, dtype=np.int32).astype(np.float32) / 2147483648.0
    else:
        raise RuntimeError(f"Unsupported WAV sample width: {sampwidth*8} bits")
    if ch == 2:
        x = x.reshape(-1, 2).mean(axis=1)  # stereo to mono
    elif ch != 1:
        raise RuntimeError(f"Unsupported channels: {ch}")
    return x.astype(np.float32, copy=False), sr

class PersistentPlayoutClient:
    def __init__(self, host: str = "127.0.0.1", port: int = 49160):
        self.host = host
        self.port = port
        self.sock: socket.socket | None = None

    def connect(self):
        if self.sock is not None:
            return
        self.sock = socket.create_connection((self.host, self.port))
        self.sock.sendall(b"PCM48K_F32LE_MONO\n")

    def send_frames(self, f32_audio: np.ndarray):
        if self.sock is None:
            self.connect()
        for frame in chunk_frames_f32(f32_audio, SAMPLES_PER_FRAME):
            self.sock.sendall(frame.tobytes(order="C"))

    def close(self):
        try:
            if self.sock:
                self.sock.shutdown(socket.SHUT_WR)
                self.sock.close()
        finally:
            self.sock = None

def fetch_tts_wav(text: str, lang: str, server_port: int) -> bytes:
    base = f"http://127.0.0.1:{server_port}"
    url = f"{base}/tts_proxy?lang={lang}&text={requests.utils.quote(text)}"
    r = requests.get(url, timeout=60)
    r.raise_for_status()
    return r.content

def send_text(client: PersistentPlayoutClient, text: str, lang: str, server_port: int):
    if not text.strip():
        return
    wav_bytes = fetch_tts_wav(text, lang, server_port)
    x, sr_in = wav_bytes_to_f32_mono(wav_bytes)
    x48 = resample_linear(x, sr_in, SR_TARGET)
    client.send_frames(x48)
    # separator between utterances:
    client.send_frames(np.zeros(int(0.05 * SR_TARGET), dtype=np.float32))  # 50 ms silence

if __name__ == "__main__":
    SERVER_PORT = int(os.getenv("RUST_HTTP_PORT", "8081"))  
    PLAYOUT_PORT = int(os.getenv("PLAYOUT_PORT", "49160"))
    LANG = os.getenv("TGT_LANG", "en")

    client = PersistentPlayoutClient(port=PLAYOUT_PORT)
    client.connect()

    try:
        # multiple utterances in one run
        demo_lines = [
            "Hello from Coqui via /tts_proxy.",
            "Zoom should hear only this translated voice.",
            "This is being sent over one persistent socket."
        ]
        for line in demo_lines:
            send_text(client, line, LANG, SERVER_PORT)

    finally:
        client.close()
