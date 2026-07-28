"""Sidecar de voz sintetica para LiveTranscriber.

Hermano de `mt_server.py`: mismo protocolo de frames, pero texto a audio en
vez de texto a texto. Recibe frases ya traducidas y devuelve la voz que las
pronuncia, para que Rust las escriba en el microfono virtual.

Dos motores:

- **chatterbox** (ResembleAI, MIT): clona una voz a partir de un WAV de
  referencia. 23 idiomas, ~3,4 GB de VRAM, y en la 3060 va justo en el filo
  de tiempo real, por eso Rust agrupa frases antes de pedir.
- **kokoro** (hexgrad, Apache 2.0): voces preajustadas, sin clonar. 8 idiomas,
  ~0,6 GB y 40x tiempo real medido; la opcion ligera.

La voz de referencia de chatterbox se codifica UNA vez al arrancar
(`prepare_conditionals`) en vez de en cada peticion: medido en esta maquina
son 244 ms menos por frase, y es justo lo que la pasa de 0,97x a 1,03x de
tiempo real. Ese 6% es la diferencia entre un retraso acotado y uno que
crece sin limite.

No hay `--dtype` como en los otros sidecars: chatterbox gestiona su precision
internamente y kokoro es float32; un flag que no hace nada solo confundiria.

Protocolo
---------
stdin (binario, poco endian):

    u32 longitud | u8 tipo | payload[longitud]

    tipo 0x02  control: JSON utf-8
        {"cmd": "speak", "id": 7, "text": "...", "lang": "en"}
        {"cmd": "shutdown"}

stdout: una linea JSON por mensaje.

    {"t": "ready", "device": "cuda", "engine": "chatterbox", "rate": 24000}
    {"t": "audio", "id": 7, "pcm": "<base64 i16 LE mono>", "rate": 24000, "ms": 4560}
    {"t": "error", "id": 7, "message": "..."}

El `id` viaja de vuelta para emparejar respuesta con peticion. El audio va en
base64 dentro de la linea JSON a proposito: mantiene el protocolo de una linea
por mensaje, y el 33% de sobrecoste da igual al lado de los segundos que tarda
la sintesis.
"""

import argparse
import base64
import json
import os
import struct
import sys
import threading
import time
import warnings

# Antes de cualquier import que arrastre numba (librosa, via chatterbox).
# En Windows, numba activa la vectorizacion Intel SVML si cree que existe,
# pero sin `svml_dispmd.dll` en el PATH el primer coseno jiteado ABORTA el
# proceso entero con "LLVM ERROR: Symbol not found: __svml_cosf8_ha" — sin
# excepcion de Python que atrapar. Se desactiva aqui y no via entorno de la
# maquina porque el sidecar tiene que funcionar lo lance quien lo lance.
# El coste es inapreciable: SVML vectoriza extraccion de rasgos de audio,
# no la inferencia en GPU.
os.environ.setdefault("NUMBA_DISABLE_INTEL_SVML", "1")

import numpy as np
import torch

FRAME_CONTROL = 0x02

# Idiomas que acepta Chatterbox Multilingual. Un codigo fuera de esta lista
# genera audio en un idioma equivocado sin avisar, asi que mejor validar.
CHATTERBOX_LANGS = {
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "it", "ja",
    "ko", "ms", "nl", "no", "pl", "pt", "ru", "sv", "sw", "tr", "zh",
}

# Kokoro llama a los idiomas por una letra. Solo tiene estos ocho.
KOKORO_LANGS = {
    "en": "a",  # 'a' = ingles americano; 'b' seria el britanico
    "es": "e",
    "fr": "f",
    "hi": "h",
    "it": "i",
    "pt": "p",
    "ja": "j",
    "zh": "z",
}

warnings.filterwarnings("ignore")

# El protocolo vive en el stdout ORIGINAL; a las librerias se les quita.
#
# Chatterbox imprime cosas a stdout durante `generate` (reproducido: una
# linea suelta rompia el JSON del protocolo y tiraba al lector de Rust).
# Cazar cada print de cada libreria es una batalla perdida: se duplica el
# stdout real para el protocolo y se redirige el descriptor 1 a stderr, con
# lo que cualquier print — incluso desde codigo C — acaba en el log.
_protocol_out = os.fdopen(os.dup(1), "wb")
os.dup2(2, 1)

_stdout_lock = threading.Lock()


def emit(obj: dict) -> None:
    line = (json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8")
    with _stdout_lock:
        _protocol_out.write(line)
        _protocol_out.flush()


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


def to_pcm16_b64(audio: np.ndarray) -> str:
    """f32 [-1, 1] -> i16 little-endian -> base64."""
    clipped = np.clip(audio, -1.0, 1.0)
    pcm = (clipped * 32767.0).astype("<i2")
    return base64.b64encode(pcm.tobytes()).decode("ascii")


class ChatterboxEngine:
    """Chatterbox Multilingual con la voz de referencia pre-codificada."""

    # Los mismos ajustes que usa voicebox por defecto; con ellos se midio
    # la calidad que dio el visto bueno a la clonacion.
    EXAGGERATION = 0.5
    CFG_WEIGHT = 0.5
    TEMPERATURE = 0.8
    REPETITION_PENALTY = 1.2

    # Deteccion de truncado. El muestreo de chatterbox a veces emite el fin
    # de secuencia antes de terminar un bloque multi-frase: es loteria de la
    # semilla, no de la configuracion (aislado con una matriz eager x
    # conditionals x semilla: la semilla 1234 truncaba en las cuatro
    # combinaciones y la 99 completaba en las cuatro). Medido en el bloque de
    # prueba: completo 47,6 ms por caracter, truncado 30,8. Por debajo del
    # umbral se reintenta (el RNG avanza solo, cada intento es otra tirada)
    # y se queda el audio mas largo.
    MIN_MS_PER_CHAR = 38
    MAX_ATTEMPTS = 3

    def __init__(self, device: str, voice_wav: str) -> None:
        from chatterbox.mtl_tts import ChatterboxMultilingualTTS

        self._patch_repetition_detector()

        log(f"cargando chatterbox en {device} (en frio tarda ~21 s)...")
        started = time.perf_counter()
        self.model = ChatterboxMultilingualTTS.from_pretrained(device=device)
        log(f"modelo cargado en {time.perf_counter() - started:.1f}s")

        # El analizador de alineacion de chatterbox necesita los pesos de
        # atencion, y la implementacion sdpa de transformers ignora
        # `output_attentions` en silencio (se ve como un aviso "not valid and
        # may be ignored"). Sin pesos el analizador queda a medias y el
        # proceso unas veces genera y otras muere sin traceback (reproducido
        # aqui). Forzar `eager` es el mismo arreglo que aplica voicebox.
        tfmr = self.model.t3.tfmr
        if hasattr(tfmr, "config") and hasattr(tfmr.config, "_attn_implementation"):
            tfmr.config._attn_implementation = "eager"
            for layer in getattr(tfmr, "layers", []):
                if hasattr(layer, "self_attn"):
                    layer.self_attn._attn_implementation = "eager"

        # Una sola vez, no por peticion: 244 ms menos por frase (medido), y
        # es lo que cruza el umbral de tiempo real.
        log(f"codificando la voz de referencia {voice_wav}...")
        self.model.prepare_conditionals(voice_wav, exaggeration=self.EXAGGERATION)
        log("voz de referencia lista")

        self.rate = int(getattr(self.model, "sr", 24000))

    @staticmethod
    def _patch_repetition_detector() -> None:
        """Neutraliza un detector roto del analizador de chatterbox.

        En `AlignmentStreamAnalyzer.step` hay un corte por "repeticion
        excesiva de tokens" cuyo comentario dice "3x same token in a row"
        pero cuyo codigo mira solo los DOS ultimos tokens, y con la guarda
        `self.complete and` comentada en la propia libreria: cualquier token
        repetido dos veces seguidas fuerza el fin de la generacion, en
        cualquier punto. El habla real repite tokens con normalidad (una
        pausa entre frases son tokens de silencio identicos), asi que textos
        multi-frase salen decapitados; reproducido aqui con un bloque cuyas
        tres tiradas murieron en el mismo sitio, la frontera de la primera
        frase.

        El parche recorta la ventana `generated_tokens` antes de cada paso,
        con lo que ese contador nunca llega a los 3 que exige y el corte roto
        no dispara. Los detectores basados en alineacion (`long_tail`,
        `alignment_repetition`), que son los que estan bien hechos, siguen
        activos. El tope de tokens del modelo acota cualquier descarrile
        real, y el reintento por ms/caracter de arriba queda como red.
        """
        from chatterbox.models.t3.inference import alignment_stream_analyzer as asa

        original = asa.AlignmentStreamAnalyzer.step

        def step(self, logits, next_token=None):
            tokens = getattr(self, "generated_tokens", None)
            if tokens is not None and len(tokens) > 1:
                self.generated_tokens = tokens[-1:]
            return original(self, logits, next_token)

        asa.AlignmentStreamAnalyzer.step = step
        log("detector de repeticion de chatterbox neutralizado (ventana de 2 tokens)")

    def _generate_once(self, text: str, lang: str) -> np.ndarray:
        # Sin audio_prompt_path: la voz ya esta preparada. Si se pasara aqui,
        # se re-codificaria en cada frase y se perderia la ganancia.
        wav = self.model.generate(
            text,
            language_id=lang,
            cfg_weight=self.CFG_WEIGHT,
            temperature=self.TEMPERATURE,
            repetition_penalty=self.REPETITION_PENALTY,
        )
        # Chatterbox trae su propio corte de descarrilamiento: cuando detecta
        # tokens repetidos fuerza el fin de la generacion el solo (se ve como
        # "Detected 2x repetition" en este stderr). No hay que anadir otro.
        if isinstance(wav, torch.Tensor):
            wav = wav.squeeze().float().cpu().numpy()
        return np.asarray(wav, dtype=np.float32).squeeze()

    def speak(self, text: str, lang: str) -> np.ndarray:
        if lang not in CHATTERBOX_LANGS:
            raise ValueError(f"chatterbox no habla {lang!r}")
        best = None
        for attempt in range(self.MAX_ATTEMPTS):
            audio = self._generate_once(text, lang)
            if best is None or len(audio) > len(best):
                best = audio
            ms_per_char = (len(audio) / self.rate * 1000) / max(len(text), 1)
            if ms_per_char >= self.MIN_MS_PER_CHAR:
                if attempt:
                    log(f"truncado recuperado al intento {attempt + 1}")
                return audio
            log(
                f"posible truncado ({ms_per_char:.0f} ms/char, umbral "
                f"{self.MIN_MS_PER_CHAR}), reintentando..."
            )
        # Todas las tiradas quedaron cortas: se entrega la mas larga. Perder
        # el final de una frase es malo; no decir nada seria peor.
        log("todos los intentos quedaron cortos, se entrega el mas largo")
        return best


class KokoroEngine:
    """Kokoro 82M: voces preajustadas, una tuberia por idioma."""

    def __init__(self, device: str, voice: str) -> None:
        from kokoro import KPipeline

        self._KPipeline = KPipeline
        self.device = device
        self.voice = voice
        # Las tuberias se crean al primer uso de cada idioma y se reutilizan:
        # crear una por peticion tiraria la ventaja de velocidad de kokoro.
        self._pipelines: dict[str, object] = {}
        self.rate = 24000
        log(f"kokoro listo (voz {voice}); los modelos se cargan al primer uso")

    def _pipeline(self, lang: str):
        code = KOKORO_LANGS.get(lang)
        if code is None:
            raise ValueError(
                f"kokoro no habla {lang!r} (tiene: {', '.join(sorted(KOKORO_LANGS))})"
            )
        if code not in self._pipelines:
            log(f"cargando la tuberia de kokoro para {lang!r}...")
            self._pipelines[code] = self._KPipeline(
                lang_code=code, repo_id="hexgrad/Kokoro-82M", device=self.device
            )
        return self._pipelines[code]

    def speak(self, text: str, lang: str) -> np.ndarray:
        pipeline = self._pipeline(lang)
        # La voz lleva el idioma en el prefijo (ef_ = espanol femenino). Usar
        # una voz de otro idioma funciona pero suena con acento; se avisa una
        # vez por si es un descuido y no una eleccion.
        expected = KOKORO_LANGS.get(lang, "?")
        if not self.voice.startswith(expected):
            log(f"aviso: la voz {self.voice!r} no es del idioma {lang!r}")
        chunks = [
            result.audio.squeeze().cpu().numpy()
            for result in pipeline(text, voice=self.voice)
            if result.audio is not None
        ]
        if not chunks:
            raise RuntimeError("kokoro no devolvio audio")
        return np.concatenate(chunks).astype(np.float32)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--engine", default="chatterbox", choices=["chatterbox", "kokoro"])
    ap.add_argument("--voice-wav", default=None, help="WAV de referencia (chatterbox)")
    ap.add_argument("--kokoro-voice", default="af_heart", help="voz preajustada (kokoro)")
    ap.add_argument(
        "--warm-lang",
        default=None,
        help="idioma a precalentar antes del ready; kokoro carga su tuberia al "
        "primer uso (~3 s medidos) y ese coste va mejor en el arranque que "
        "comido por la primera frase",
    )
    ap.add_argument("--device", default=None, help="cuda | cpu")
    args = ap.parse_args()

    device = args.device or ("cuda" if torch.cuda.is_available() else "cpu")

    if args.engine == "chatterbox":
        if not args.voice_wav:
            # Fallar aqui, claro y al arrancar, no en la primera frase.
            log("error: chatterbox necesita --voice-wav con la voz a clonar")
            sys.exit(2)
        # Validar el WAV ANTES de cargar 3,4 GB de modelo. Sin esto, un WAV
        # corrupto muere en librosa con un NoBackendError cuyo str() es
        # literalmente vacio, y uno de 0 muestras revienta despues dentro de
        # chatterbox con un error de dimensiones de torch: ninguno de los dos
        # apunta al fichero. Reproducido; por eso este peaje.
        import soundfile as sf

        try:
            info = sf.info(args.voice_wav)
        except Exception as exc:
            log(f"error: no se puede leer la muestra de voz {args.voice_wav!r}: {exc!r}")
            log("¿es un WAV de verdad? Exportalo como WAV PCM de 16 bits.")
            sys.exit(2)
        if info.frames == 0:
            log(f"error: la muestra de voz {args.voice_wav!r} esta vacia (0 muestras)")
            sys.exit(2)
        seconds = info.frames / max(info.samplerate, 1)
        if seconds < 1.0:
            log(
                f"aviso: la muestra de voz dura {seconds:.2f}s; con menos de "
                "unos 10 s la clonacion sale pobre"
            )
        try:
            engine = ChatterboxEngine(device, args.voice_wav)
        except Exception as exc:
            log(f"error cargando chatterbox con {args.voice_wav!r}: {exc!r}")
            sys.exit(2)
    else:
        engine = KokoroEngine(device, args.kokoro_voice)
        if args.warm_lang in KOKORO_LANGS:
            # Ademas de la tuberia se sintetiza una palabra: fuerza la
            # descarga del fichero de la voz, asi que una voz mal escrita
            # falla aqui con un error claro y no en la primera frase de la
            # reunion.
            try:
                engine._pipeline(args.warm_lang)
                engine.speak("ok", args.warm_lang)
            except Exception as exc:
                log(f"error: la voz {args.kokoro_voice!r} no funciona: {exc!r}")
                sys.exit(2)

    emit({"t": "ready", "device": device, "engine": args.engine, "rate": engine.rate})
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
        if cmd != "speak":
            emit({"t": "error", "id": msg.get("id", 0), "message": f"comando desconocido: {cmd!r}"})
            continue

        request_id = msg.get("id", 0)
        text = (msg.get("text") or "").strip()
        if not text:
            emit({"t": "audio", "id": request_id, "pcm": "", "rate": engine.rate, "ms": 0})
            continue

        started = time.perf_counter()
        try:
            audio = engine.speak(text, msg.get("lang", "en"))
            # Un tensor envenenado de NaN (le pasa a los modelos en fp16 con
            # mala suerte) sobreviviria al clip de to_pcm16_b64 como ceros:
            # silencio absoluto entregado como audio valido, sin rastro en
            # ningun log (el RuntimeWarning de numpy esta silenciado arriba).
            # Mejor un error visible que una frase muda.
            if not np.isfinite(audio).all():
                raise RuntimeError(
                    f"el motor devolvio audio no finito "
                    f"({int(np.isnan(audio).sum())} NaN de {audio.size} muestras)"
                )
        except Exception as exc:  # una frase mala no debe tumbar el proceso
            emit({"t": "error", "id": request_id, "message": f"{type(exc).__name__}: {exc}"})
            log(f"error sintetizando: {exc!r}")
            continue

        emit(
            {
                "t": "audio",
                "id": request_id,
                "pcm": to_pcm16_b64(audio),
                "rate": engine.rate,
                "ms": round((time.perf_counter() - started) * 1000),
            }
        )

    log("sidecar de voz terminado")


if __name__ == "__main__":
    main()
