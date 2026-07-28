"""Sidecar de traduccion para LiveTranscriber.

Hermano de `asr_server.py`: mismo protocolo de frames, pero texto a texto en
vez de audio a texto. Uno solo da servicio a todas las fuentes, asi que el
modelo esta una unica vez en VRAM.

Traduce con NLLB-200. Ojo con la licencia: **CC-BY-NC-4.0, uso no comercial**.

Protocolo
---------
stdin (binario, poco endian):

    u32 longitud | u8 tipo | payload[longitud]

    tipo 0x02  control: JSON utf-8
        {"cmd": "translate", "id": 7, "text": "...", "src": "spa_Latn", "tgt": "eng_Latn"}
        {"cmd": "shutdown"}

stdout: una linea JSON por mensaje.

    {"t": "ready", "device": "cuda", "languages": 202}
    {"t": "translation", "id": 7, "text": "...", "ms": 180}
    {"t": "error", "id": 7, "message": "..."}

El `id` viaja de vuelta para poder emparejar respuesta con peticion sin asumir
que llegan en orden.
"""

import argparse
import json
import struct
import sys
import threading
import time
import warnings

import torch
from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

MODEL_ID = "facebook/nllb-200-distilled-600M"

FRAME_CONTROL = 0x02

warnings.filterwarnings("ignore", message=".*max_length.*")

# NLLB trae max_length=200 en su generation_config, asi que pasar
# max_new_tokens dispara un aviso en cada frase. No va por `warnings` sino por
# el logger de transformers, asi que hay que callarlo por aqui.
from transformers.utils import logging as hf_logging  # noqa: E402

hf_logging.set_verbosity_error()

_stdout_lock = threading.Lock()


def emit(obj: dict) -> None:
    line = (json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8")
    with _stdout_lock:
        sys.stdout.buffer.write(line)
        sys.stdout.buffer.flush()


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def read_exact(stream, n: int) -> bytes | None:
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
    """Igual que en `asr_server.py`: bfloat16 necesita Ampere o superior.

    En Turing o Volta PyTorch lo emula en vez de fallar, y eso deja la app
    lentisima sin ninguna pista de por que. Mejor bajar a float16 avisando.
    """
    if device == "cpu":
        return torch.float32

    dtype = getattr(torch, requested)
    if dtype is torch.bfloat16 and torch.cuda.get_device_capability()[0] < 8:
        log(
            f"aviso: {torch.cuda.get_device_name()} no tiene bfloat16 nativo; "
            "usando float16"
        )
        return torch.float16
    return dtype


class Translator:
    def __init__(self, device: str, dtype: torch.dtype) -> None:
        log(f"cargando NLLB en {device} ({dtype})...")
        self.tokenizer = AutoTokenizer.from_pretrained(MODEL_ID)
        self.model = AutoModelForSeq2SeqLM.from_pretrained(MODEL_ID, dtype=dtype).to(device)
        self.model.eval()
        self.unk = self.tokenizer.unk_token_id

    def language_id(self, code: str) -> int | None:
        """Id del token de idioma, o None si NLLB no conoce ese codigo."""
        token_id = self.tokenizer.convert_tokens_to_ids(code)
        if token_id is None or token_id == self.unk:
            return None
        return token_id

    def translate(self, text: str, src: str, tgt: str) -> str:
        target_id = self.language_id(tgt)
        if target_id is None:
            raise ValueError(f"NLLB no conoce el idioma destino {tgt!r}")
        if self.language_id(src) is None:
            raise ValueError(f"NLLB no conoce el idioma origen {src!r}")

        self.tokenizer.src_lang = src
        inputs = self.tokenizer(text, return_tensors="pt", truncation=True, max_length=512)
        inputs = inputs.to(self.model.device)
        with torch.inference_mode():
            output = self.model.generate(
                **inputs,
                forced_bos_token_id=target_id,
                max_new_tokens=256,
                num_beams=1,  # greedy: esto va en vivo, la latencia manda
            )
        return self.tokenizer.batch_decode(output, skip_special_tokens=True)[0]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--device", default=None, help="cuda | cpu")
    ap.add_argument("--dtype", default="bfloat16", choices=["bfloat16", "float16", "float32"])
    args = ap.parse_args()

    device = args.device or ("cuda" if torch.cuda.is_available() else "cpu")
    dtype = pick_dtype(device, args.dtype)

    translator = Translator(device, dtype)
    emit({"t": "ready", "device": device, "dtype": str(dtype).replace("torch.", "")})
    log("listo")

    stream = sys.stdin.buffer
    while True:
        frame = read_frame(stream)
        if frame is None:
            break
        frame_type, payload = frame
        if frame_type != FRAME_CONTROL:
            emit({"t": "error", "id": 0, "message": f"tipo de frame inesperado: {frame_type}"})
            continue

        try:
            msg = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            emit({"t": "error", "id": 0, "message": f"control ilegible: {exc}"})
            continue

        cmd = msg.get("cmd")
        if cmd == "shutdown":
            break
        if cmd != "translate":
            emit({"t": "error", "id": msg.get("id", 0), "message": f"comando desconocido: {cmd!r}"})
            continue

        request_id = msg.get("id", 0)
        text = (msg.get("text") or "").strip()
        if not text:
            emit({"t": "translation", "id": request_id, "text": "", "ms": 0})
            continue

        started = time.perf_counter()
        try:
            translated = translator.translate(text, msg["src"], msg["tgt"])
        except Exception as exc:  # una frase mala no debe tumbar el proceso
            emit({"t": "error", "id": request_id, "message": f"{type(exc).__name__}: {exc}"})
            log(f"error traduciendo: {exc!r}")
            continue

        emit(
            {
                "t": "translation",
                "id": request_id,
                "text": translated,
                "ms": round((time.perf_counter() - started) * 1000),
            }
        )

    log("sidecar de traduccion terminado")


if __name__ == "__main__":
    main()
