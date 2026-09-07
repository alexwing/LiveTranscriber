"""Turn the latency marks in the log into a per-sentence table.

Every stage of the pipeline writes one line with ``target: "latency"``, a
sentence ``id`` and a ``stage`` name.  This script groups those lines by id
and prints how long each sentence spent between stages, plus medians and p90
across the session.  It changes nothing; it only reads the log.

    python scripts/latency_report.py                 # newest log
    python scripts/latency_report.py path/to/log     # a specific one
    python scripts/latency_report.py --last 20       # only the last 20 sentences
    python scripts/latency_report.py --all           # every session in the file, not just the last

Stages, in order.  The microphone path runs through all of them; the room
path (what the others say, which is read and never spoken) stops at
``emitted``:

    closed       the recognizer closed the sentence.  ``idle_ms`` is how long
                 the speaker had been quiet before the paragraph was cut
                 (flush only: a sentence closed by its own punctuation waits
                 for nothing).  ``queue_wait_ms`` is how long the closing
                 event sat in the translator's queue: one thread serves both
                 sources, so the microphone waits while the room is being
                 translated.  Neither includes the recognizer's own emission
                 delay (the lookahead), which is not visible from here.
    translated   NLLB returned
    emitted      the translation reached the window          (room path ends)
    speech_rx    the voice pump received the text
    synth_start  synthesis began (``waited_ms`` is the grouping wait)
    synth_done   PCM came back
    audio_start  the first byte was written to the output device
    audio_end    the last byte was written

"audio_start" is the write into WASAPI's buffer, not the speaker cone: the
shared-mode buffer adds a constant 10-30 ms that is not measured here.
"""

import argparse
import os
import re
import statistics
import sys
from datetime import datetime
from pathlib import Path

# Orden canonico de las etapas; la tabla y las deltas se apoyan en el.
MIC_STAGES = [
    "closed",
    "translated",
    "speech_rx",
    "synth_start",
    "synth_done",
    "audio_start",
    "audio_end",
]
ROOM_STAGES = ["closed", "translated", "emitted"]

# Una linea del log con target `latency`. El formato lo pone
# tracing_subscriber::fmt: marca ISO, nivel, target, y luego los campos como
# `clave=valor`, con las cadenas entre comillas.
LINE = re.compile(r"^(\S+)\s+\w+\s+latency:\s+(.*)$")
FIELD = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')


def newest_log() -> Path:
    base = Path(os.environ.get("APPDATA", "")) / "LiveTranscriber" / "logs"
    files = sorted(base.glob("live-transcriber*.log*"), key=lambda p: p.stat().st_mtime)
    if not files:
        sys.exit(f"no log found under {base}")
    return files[-1]


def parse(path: Path) -> list[dict[int, dict]]:
    """One entry per application start: id -> {"stages": {...}, "fields": {...}}.

    The log rolls daily and the sentence counter restarts at 1 with every
    process, so a single file holds several sessions.  A new session begins
    whenever an id that was already closed is closed again.
    """
    runs: list[dict[int, dict]] = [{}]
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        m = LINE.match(raw)
        if not m:
            continue
        when = datetime.fromisoformat(m.group(1).replace("Z", "+00:00"))
        fields = {}
        for key, value in FIELD.findall(m.group(2)):
            fields[key] = value.strip('"')
        try:
            sid = int(fields["id"])
            stage = fields["stage"]
        except (KeyError, ValueError):
            continue
        if stage == "closed" and "closed" in runs[-1].get(sid, {}).get("stages", {}):
            runs.append({})
        entry = runs[-1].setdefault(sid, {"stages": {}, "fields": {}})
        # La primera vez que se ve una etapa es la que vale: el agrupador
        # repite synth_* para cada frase del bloque, pero es el mismo instante.
        entry["stages"].setdefault(stage, when)
        entry["fields"].update({k: v for k, v in fields.items() if k not in ("id", "stage")})
    return [run for run in runs if run]


def ms(a: datetime | None, b: datetime | None) -> int | None:
    if a is None or b is None:
        return None
    return round((b - a).total_seconds() * 1000)


def fmt(value: int | None, width: int = 7) -> str:
    return f"{value:>{width}}" if value is not None else f"{'-':>{width}}"


def report(sentences: dict[int, dict], last: int | None) -> None:
    ids = sorted(sentences)
    if last:
        ids = ids[-last:]
    if not ids:
        print("no latency marks in this log (translation off, or nothing was said)")
        return

    # Deltas entre etapas consecutivas, por frase. `total` es de cerrar la
    # frase a empezar a sonar, que es lo que el otro extremo percibe.
    rows = []
    for sid in ids:
        s = sentences[sid]["stages"]
        f = sentences[sid]["fields"]
        path = "mic" if "speech_rx" in s or "synth_start" in s else "room"
        rows.append(
            {
                "id": sid,
                "path": path,
                "src": f.get("source", "?"),
                "by": f.get("closed_by", "?"),
                # Solo un flush espera a que el hablante calle; en punct no
                # hay nada que medir y se pinta "-", no 0.
                "idle": int(f.get("idle_ms", 0) or 0) if f.get("closed_by") == "flush" else None,
                "wait": int(f.get("queue_wait_ms", 0) or 0),
                "translate": ms(s.get("closed"), s.get("translated")),
                "emit": ms(s.get("translated"), s.get("emitted")),
                "handoff": ms(s.get("translated"), s.get("speech_rx")),
                "group": ms(s.get("speech_rx"), s.get("synth_start")),
                "grouped": f.get("grouped"),
                "synth": ms(s.get("synth_start"), s.get("synth_done")),
                "queue": ms(s.get("synth_done"), s.get("audio_start")),
                "audio": int(f.get("audio_ms", 0) or 0) if "audio_ms" in f else None,
                "chars": f.get("chars", "?"),
            }
        )
        # TOTAL suma lo que paso ANTES de `closed`: la espera a que el hablante
        # callara (idle) y la cola del traductor (wait). `closed` se marca
        # cuando la bomba llega al evento, no cuando se emitio, asi que sin
        # sumarlas esas dos esperas no las veria nadie.
        r = rows[-1]
        end = ms(s.get("closed"), s.get("audio_start" if path == "mic" else "emitted"))
        r["total"] = None if end is None else end + r["wait"] + (r["idle"] or 0)

    print(
        f"{'id':>4} {'path':<4} {'src':<6} {'by':<5} {'idle':>7} {'wait':>7} {'transl':>7} "
        f"{'emit':>7} {'handoff':>7} {'group':>7} {'n':>2} {'synth':>7} {'queue':>7} "
        f"{'audio':>7} {'TOTAL':>7}  chars"
    )
    print("-" * 112)
    for r in rows:
        print(
            f"{r['id']:>4} {r['path']:<4} {r['src']:<6} {r['by']:<5} {fmt(r['idle'])} "
            f"{fmt(r['wait'])} {fmt(r['translate'])} {fmt(r['emit'])} {fmt(r['handoff'])} "
            f"{fmt(r['group'])} {(r['grouped'] or '-'):>2} {fmt(r['synth'])} {fmt(r['queue'])} "
            f"{fmt(r['audio'])} {fmt(r['total'])}  {r['chars']}"
        )

    print("\nall times in ms. idle = quiet time before the paragraph was cut (flush only); "
          "wait = closing event queued behind the other source; TOTAL = idle + wait + "
          "closed -> first audio out (mic) or on screen (room)\n")

    for path in ("mic", "room"):
        subset = [r for r in rows if r["path"] == path]
        if not subset:
            continue
        print(f"{path} path, {len(subset)} sentences - median / p90:")
        for key in ("idle", "wait", "translate", "emit", "handoff", "group", "synth", "queue", "total"):
            values = [r[key] for r in subset if r[key] is not None]
            if len(values) < 1:
                continue
            med = statistics.median(values)
            # ceil, no int: con pocas muestras `int(n*0.9)-1` caia en el
            # primer elemento y el p90 salia por debajo de la mediana.
            ordered = sorted(values)
            p90 = ordered[min(len(ordered) - 1, max(0, -(-9 * len(ordered) // 10) - 1))]
            print(f"  {key:<10} {med:>7.0f} / {p90:>6}   (n={len(values)})")
        print()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", nargs="?", help="log file (default: the newest one)")
    ap.add_argument("--last", type=int, help="only the last N sentences")
    ap.add_argument("--all", action="store_true", help="every session in the log, not just the last")
    args = ap.parse_args()
    path = Path(args.log) if args.log else newest_log()
    print(f"log: {path}")
    runs = parse(path)
    if not runs:
        print("\nno latency marks in this log (translation off, or nothing was said)")
        return
    shown = runs if args.all else runs[-1:]
    for run in shown:
        index = runs.index(run) + 1
        first = min(e["stages"]["closed"] for e in run.values() if "closed" in e["stages"])
        print(f"\n== session {index}/{len(runs)}, first sentence at {first:%H:%M:%S} ==\n")
        report(run, args.last)


if __name__ == "__main__":
    main()
