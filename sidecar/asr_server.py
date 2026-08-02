"""Sidecar de reconocimiento de voz para LiveTranscriber.

Lee audio PCM por stdin y escribe transcripcion por stdout. Es un proceso
tonto a proposito: no sabe nada de dispositivos ni de ventanas, solo convierte
muestras en texto. Toda la logica de captura vive en Rust.

Protocolo
---------
stdin (binario, poco endian):

    u32 longitud | u8 tipo | payload[longitud]

    tipo 0x01  PCM: muestras f32 mono a 16 kHz
    tipo 0x02  control: JSON utf-8, {"cmd": "reset" | "shutdown"}

stdout: una linea JSON por mensaje.

    {"t": "ready",       "latency_ms": 320, ...}
    {"t": "delta",       "text": "hola "}
    {"t": "segment_end"}
    {"t": "error",       "message": "..."}

stderr: trazas para humanos, no lo parsea nadie.

El "reset" cierra el segmento en curso y arranca uno nuevo con el estado del
modelo limpio. Rust lo manda cuando el gate detecta un silencio largo.
"""

import argparse
import json
import queue
import struct
import sys
import threading
import warnings

import numpy as np
import torch
from transformers import AutoModelForRNNT, AutoProcessor, TextIteratorStreamer

MODEL_ID = "nvidia/nemotron-3.5-asr-streaming-0.6b"

FRAME_PCM = 0x01
FRAME_CONTROL = 0x02

SUPPORTED_LOOKAHEAD = [0, 3, 6, 13]

# Avisos benignos: max_length lo deriva transformers de la duracion y no trunca;
# el de cuDNN sale porque su ruta fusionada de RNN no cubre bfloat16.
warnings.filterwarnings("ignore", message=".*max_length.*")
warnings.filterwarnings("ignore", message=".*not part of single contiguous chunk.*")

_stdout_lock = threading.Lock()


def emit(obj: dict) -> None:
    """Escribe un mensaje por stdout. Se usa desde varios hilos."""
    line = (json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8")
    with _stdout_lock:
        sys.stdout.buffer.write(line)
        sys.stdout.buffer.flush()


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def read_exact(stream, n: int) -> bytes | None:
    """Lee exactamente n bytes, o None si el otro extremo cerro."""
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


def read_frame(stream) -> tuple[int, bytes] | None:
    header = read_exact(stream, 5)
    if header is None:
        return None
    (length,) = struct.unpack("<I", header[:4])
    frame_type = header[4]
    payload = b""
    if length:
        payload = read_exact(stream, length)
        if payload is None:
            return None
    return frame_type, payload


def pick_dtype(device: str, requested: str) -> torch.dtype:
    """Precision a usar de verdad, avisando si la pedida no es nativa.

    bfloat16 necesita Ampere (capability 8.0) o superior. En una Turing (2080,
    1660) o una Volta, PyTorch **no falla**: lo emula. Y una emulacion silenciosa
    es peor que un error, porque el usuario ve la app lentisima sin saber por que.
    Asi que aqui se detecta y se baja a float16, que esas tarjetas si tienen en
    hardware.
    """
    if device == "cpu":
        return torch.float32

    dtype = getattr(torch, requested)
    if dtype is torch.bfloat16:
        major = torch.cuda.get_device_capability()[0]
        if major < 8:
            name = torch.cuda.get_device_name()
            log(
                f"aviso: {name} (capability {major}.x) no tiene bfloat16 nativo, "
                "hace falta Ampere o superior. Usando float16."
            )
            return torch.float16
    return dtype


def flatten_rnns(model: torch.nn.Module) -> None:
    """cuDNN quiere los pesos recurrentes contiguos; si no, los recompacta siempre."""
    for module in model.modules():
        if isinstance(module, torch.nn.RNNBase):
            module.flatten_parameters()


class AudioBuffer:
    """Buffer deslizante indexado en muestras absolutas desde el inicio del segmento.

    Es la misma idea que el buffer del microfono: el modelo pide ventanas
    solapadas que retroceden n_fft//2 muestras, asi que hay que conservar el
    solape en vez de consumir y tirar. Con lookahead 0 la primera ventana del
    bucle pide un indice negativo, que se rellena con silencio.
    """

    def __init__(self) -> None:
        self.q: queue.Queue = queue.Queue()
        self.buf = np.zeros(0, dtype=np.float32)
        self.base = 0
        self.closed = threading.Event()

    def push(self, samples: np.ndarray) -> None:
        self.q.put(samples)

    def close(self) -> None:
        self.closed.set()
        self.q.put(None)  # despierta a quien este bloqueado en take()

    def take(self, abs_start: int, abs_end: int) -> np.ndarray | None:
        """Bloquea hasta tener [abs_start, abs_end). None si el segmento se cerro."""
        pad = max(0, -abs_start)
        abs_start = max(0, abs_start)

        while self.base + len(self.buf) < abs_end:
            try:
                item = self.q.get(timeout=0.1)
            except queue.Empty:
                if self.closed.is_set():
                    return None
                continue
            if item is None:
                return None
            self.buf = np.concatenate([self.buf, item])

        drop = abs_start - self.base
        if drop > 0:
            self.buf = self.buf[drop:]
            self.base += drop

        block = self.buf[abs_start - self.base : abs_end - self.base]
        if pad:
            block = np.concatenate([np.zeros(pad, dtype=block.dtype), block])
        return block


class Session:
    """Un segmento de transcripcion: un pase continuo de streaming del modelo."""

    def __init__(self, model, processor, language: str) -> None:
        self.model = model
        self.processor = processor
        self.language = language
        self.audio = AudioBuffer()
        self.thread: threading.Thread | None = None

    def start(self) -> None:
        self.thread = threading.Thread(target=self._run, name="asr-session", daemon=True)
        self.thread.start()

    def feed(self, samples: np.ndarray) -> None:
        self.audio.push(samples)

    def close(self, timeout: float = 15.0) -> None:
        self.audio.close()
        if self.thread is not None:
            self.thread.join(timeout=timeout)
            if self.thread.is_alive():
                log("aviso: la sesion no termino a tiempo")

    def _run(self) -> None:
        try:
            self._transcribe()
        except Exception as exc:  # el sidecar no debe morir por un segmento malo
            emit({"t": "error", "message": f"{type(exc).__name__}: {exc}"})
            log(f"error en la sesion: {exc!r}")
        finally:
            emit({"t": "segment_end"})

    def _transcribe(self) -> None:
        p = self.processor
        model = self.model
        sr = p.feature_extractor.sampling_rate
        hop = p.feature_extractor.hop_length
        n_fft = p.feature_extractor.n_fft

        # El primer chunk fija el prompt de idioma del segmento entero.
        raw = self.audio.take(0, p.num_samples_first_audio_chunk)
        if raw is None:
            return
        first = p(
            raw,
            sampling_rate=sr,
            is_streaming=True,
            is_first_audio_chunk=True,
            language=self.language,
            return_tensors="pt",
        ).to(model.device, dtype=model.dtype)

        def features():
            yield first.input_features[:, : p.num_mel_frames_first_audio_chunk, :]
            mel_idx = p.num_mel_frames_first_audio_chunk
            while True:
                start = mel_idx * hop - n_fft // 2
                block = self.audio.take(start, start + p.num_samples_per_audio_chunk)
                if block is None:
                    return
                inputs = p(
                    block,
                    sampling_rate=sr,
                    is_streaming=True,
                    is_first_audio_chunk=False,
                    language=self.language,
                    return_tensors="pt",
                ).to(model.device, dtype=model.dtype)
                yield inputs.input_features
                mel_idx += p.num_mel_frames_per_audio_chunk

        streamer = TextIteratorStreamer(p.tokenizer, skip_special_tokens=True)
        worker = threading.Thread(
            target=model.generate,
            kwargs={**first, "input_features": features(), "streamer": streamer},
            name="asr-generate",
            daemon=True,
        )
        worker.start()
        for text in streamer:
            if text:
                emit({"t": "delta", "text": text})
        worker.join(timeout=10)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--language", default="auto", help="locale (es-ES, en-US, ...) o 'auto'")
    ap.add_argument("--lookahead", type=int, default=3, choices=SUPPORTED_LOOKAHEAD)
    ap.add_argument("--device", default=None, help="cuda | cpu")
    ap.add_argument("--dtype", default="bfloat16", choices=["bfloat16", "float16", "float32"])
    args = ap.parse_args()

    device = args.device or ("cuda" if torch.cuda.is_available() else "cpu")
    dtype = pick_dtype(device, args.dtype)

    log(f"cargando modelo en {device} ({dtype})...")
    processor = AutoProcessor.from_pretrained(MODEL_ID)
    model = AutoModelForRNNT.from_pretrained(MODEL_ID, dtype=dtype).to(device)
    model.eval()
    flatten_rnns(model)
    processor.set_num_lookahead_tokens(args.lookahead)

    emit(
        {
            "t": "ready",
            "device": device,
            "dtype": str(dtype).replace("torch.", ""),
            "language": args.language,
            "lookahead": args.lookahead,
            "latency_ms": processor.streaming_latency_ms,
            "sample_rate": processor.feature_extractor.sampling_rate,
            "chunk_samples": processor.num_samples_per_audio_chunk,
            "first_chunk_samples": processor.num_samples_first_audio_chunk,
        }
    )
    log("listo")

    language = args.language
    session: Session | None = None
    stream = sys.stdin.buffer

    while True:
        frame = read_frame(stream)
        if frame is None:
            break
        frame_type, payload = frame

        if frame_type == FRAME_PCM:
            if not payload:
                continue
            # frombuffer da una vista de solo lectura sobre payload; copiamos.
            samples = np.frombuffer(payload, dtype="<f4").astype(np.float32)
            if session is None:
                session = Session(model, processor, language)
                session.start()
            session.feed(samples)

        elif frame_type == FRAME_CONTROL:
            try:
                msg = json.loads(payload.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                emit({"t": "error", "message": f"unreadable control frame: {exc}"})
                continue

            cmd = msg.get("cmd")
            if cmd == "reset":
                if session is not None:
                    session.close()
                    session = None
            elif cmd == "language":
                language = msg.get("value", language)
                if session is not None:
                    session.close()
                    session = None
                log(f"idioma cambiado a {language}")
            elif cmd == "shutdown":
                break
            else:
                emit({"t": "error", "message": f"unknown command: {cmd!r}"})

        else:
            emit({"t": "error", "message": f"unknown frame type: {frame_type}"})

    if session is not None:
        session.close()
    log("sidecar terminado")


if __name__ == "__main__":
    main()
