"""Comprueba que el sidecar de ASR funciona de verdad tras instalar.

No mide calidad de transcripcion: se le manda audio sintetico, no voz. Lo que
comprueba es la tuberia completa tal como la usa la aplicacion, que es donde
suelen estar los fallos de instalacion:

  - el interprete y las dependencias cargan
  - el modelo baja de la cache y entra en la GPU
  - el protocolo de frames por stdin va bien en los dos sentidos
  - `reset` cierra el segmento y el modelo responde

Devuelve 0 si todo pasa y 1 con un motivo si no.

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
            print(f"    (linea no JSON, se ignora: {text[:80]})")
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--python", required=True, help="interprete del venv")
    ap.add_argument("--dtype", default="bfloat16")
    ap.add_argument("--language", default="es-ES")
    args = ap.parse_args()

    script = Path(__file__).resolve().parent.parent / "sidecar" / "asr_server.py"
    if not script.is_file():
        print(f"FALLO: no encuentro {script}")
        return 1

    print(f"lanzando {script.name} con {args.dtype}...")
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
            print("FALLO: el sidecar no dijo 'ready'")
            if err.strip():
                print("--- stderr ---")
                print(err[-2000:])
            return 1
        if ready.get("t") != "ready":
            print(f"FALLO: primer mensaje inesperado: {ready}")
            return 1

        load = time.monotonic() - started
        print(f"  ready en {load:.1f}s")
        print(f"  dispositivo: {ready.get('device')}  precision: {ready.get('dtype')}")
        print(f"  latencia: {ready.get('latency_ms')} ms  chunk: {ready.get('chunk_samples')} muestras")

        if ready.get("device") != "cuda":
            print("  AVISO: corriendo en CPU, no dara tiempo real")

        # Tres segundos de audio en bloques de 100 ms, como hace la aplicacion.
        print("  mandando 3 s de audio sintetico...")
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
                print("FALLO: no llego 'segment_end' tras el reset")
                return 1
            kind = msg.get("t")
            if kind == "delta":
                deltas += 1
            elif kind == "segment_end":
                break
            elif kind == "error":
                print(f"FALLO: el sidecar dio error: {msg.get('message')}")
                return 1

        print(f"  segmento cerrado correctamente ({deltas} fragmentos de texto)")
        print("  (un tono no es voz: que no salga texto es lo normal)")

        proc.stdin.write(frame(FRAME_CONTROL, b'{"cmd":"shutdown"}'))
        proc.stdin.flush()
        proc.stdin.close()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            print("  AVISO: no se cerro solo, se mata")
            proc.kill()

        print("\nOK: la tuberia de ASR funciona")
        return 0
    finally:
        if proc.poll() is None:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
