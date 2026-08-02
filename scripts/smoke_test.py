"""Checks that the ASR sidecar really works after installing.

It does not measure transcription quality: it is fed synthetic audio, not
speech. What it checks is the full pipeline the way the application uses it,
which is where install failures usually are:

  - the interpreter and the dependencies load
  - the model comes down from the cache and onto the GPU
  - the stdin frame protocol works in both directions
  - `reset` closes the segment and the model answers

Returns 0 if everything passes and 1 with a reason if not.

    python scripts/smoke_test.py --python .venv\\Scripts\\python.exe [--dtype float16]
"""

import argparse
import json
import struct
import subprocess
import sys
import time
from pathlib import Path

FRAME_PCM = 0x01
FRAME_CONTROL = 0x02

SAMPLE_RATE = 16_000
READY_TIMEOUT = 300.0
SEGMENT_TIMEOUT = 60.0


def frame(frame_type: int, payload: bytes) -> bytes:
    return struct.pack("<I", len(payload)) + bytes([frame_type]) + payload


def tone(seconds: float) -> bytes:
    """Audio sintetico. No es voz, asi que no se espera transcripcion."""
    import math

    total = int(SAMPLE_RATE * seconds)
    samples = bytearray()
    for i in range(total):
        value = 0.2 * math.sin(2 * math.pi * 220 * i / SAMPLE_RATE)
        samples += struct.pack("<f", value)
    return bytes(samples)


def read_message(proc: subprocess.Popen, deadline: float) -> dict | None:
    """Siguiente linea JSON de stdout, o None si se agota el plazo."""
    while time.monotonic() < deadline:
        line = proc.stdout.readline()
        if not line:
            return None
        text = line.decode("utf-8", "replace").strip()
        if not text:
            continue
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            print(f"    (non-JSON line, ignored: {text[:80]})")
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--python", required=True, help="venv interpreter")
    ap.add_argument("--dtype", default="bfloat16")
    ap.add_argument("--language", default="es-ES")
    args = ap.parse_args()

    script = Path(__file__).resolve().parent.parent / "sidecar" / "asr_server.py"
    if not script.is_file():
        print(f"FAILED: cannot find {script}")
        return 1

    print(f"launching {script.name} with {args.dtype}...")
    proc = subprocess.Popen(
        [args.python, str(script), "--dtype", args.dtype, "--language", args.language],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        started = time.monotonic()
        ready = read_message(proc, started + READY_TIMEOUT)
        if ready is None:
            err = proc.stderr.read().decode("utf-8", "replace")
            print("FAILED: the sidecar never said 'ready'")
            if err.strip():
                print("--- stderr ---")
                print(err[-2000:])
            return 1
        if ready.get("t") != "ready":
            print(f"FAILED: unexpected first message: {ready}")
            return 1

        load = time.monotonic() - started
        print(f"  ready in {load:.1f}s")
        print(f"  device: {ready.get('device')}  precision: {ready.get('dtype')}")
        print(f"  latency: {ready.get('latency_ms')} ms  chunk: {ready.get('chunk_samples')} samples")

        if ready.get("device") != "cuda":
            print("  WARNING: running on CPU, it will not keep up in real time")

        # Tres segundos de audio en bloques de 100 ms, como hace la aplicacion.
        print("  sending 3 s of synthetic audio...")
        block = tone(0.1)
        for _ in range(30):
            proc.stdin.write(frame(FRAME_PCM, block))
        proc.stdin.flush()

        # Cerrar el segmento y esperar que el modelo confirme.
        proc.stdin.write(frame(FRAME_CONTROL, b'{"cmd":"reset"}'))
        proc.stdin.flush()

        deadline = time.monotonic() + SEGMENT_TIMEOUT
        deltas = 0
        while True:
            msg = read_message(proc, deadline)
            if msg is None:
                print("FAILED: no 'segment_end' arrived after the reset")
                return 1
            kind = msg.get("t")
            if kind == "delta":
                deltas += 1
            elif kind == "segment_end":
                break
            elif kind == "error":
                print(f"FAILED: the sidecar returned an error: {msg.get('message')}")
                return 1

        print(f"  segment closed correctly ({deltas} text fragments)")
        print("  (a tone is not speech: getting no text is the normal outcome)")

        proc.stdin.write(frame(FRAME_CONTROL, b'{"cmd":"shutdown"}'))
        proc.stdin.flush()
        proc.stdin.close()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            print("  WARNING: it did not exit on its own, killing it")
            proc.kill()

        print("\nOK: the ASR pipeline works")
        return 0
    finally:
        if proc.poll() is None:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
