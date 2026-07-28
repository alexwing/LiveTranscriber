"""Descarga y valida los modelos que necesita LiveTranscriber.

Se cargan de verdad en lugar de hacer un `snapshot_download` a secas, por dos
motivos:

1. `from_pretrained` baja **solo** los ficheros que usa transformers. El repo
   del ASR incluye ademas un `.nemo` de 2,4 GB que no tocamos, y una descarga
   del repo completo se lo traeria para nada.
2. Si los pesos estan corruptos o falta algo, se ve aqui y no la primera vez
   que el usuario le da a Arrancar.

Se carga en CPU: no hace falta GPU para provisionar, y asi el instalador vale
tambien en un equipo donde la tarjeta todavia no este lista.

    python scripts/fetch_models.py [--skip-translator]
"""

import argparse
import os
import sys
import time

ASR_MODEL = "nvidia/nemotron-3.5-asr-streaming-0.6b"
MT_MODEL = "facebook/nllb-200-distilled-600M"


def human(n: int) -> str:
    return f"{n / 1e9:.2f} GB"


def cache_size() -> int:
    root = os.environ.get("HF_HOME")
    if root:
        root = os.path.join(root, "hub")
    else:
        root = os.path.join(os.path.expanduser("~"), ".cache", "huggingface", "hub")
    if not os.path.isdir(root):
        return 0
    total = 0
    for base, _dirs, files in os.walk(root):
        for name in files:
            try:
                total += os.path.getsize(os.path.join(base, name))
            except OSError:
                pass
    return total


def fetch_asr() -> None:
    from transformers import AutoModelForRNNT, AutoProcessor

    print(f"[1/2] {ASR_MODEL}")
    processor = AutoProcessor.from_pretrained(ASR_MODEL)
    model = AutoModelForRNNT.from_pretrained(ASR_MODEL)
    params = sum(p.numel() for p in model.parameters())
    print(f"      {params / 1e6:.0f}M parametros")
    print(f"      {len(processor.prompt_dictionary)} idiomas")
    print(f"      latencias: {sorted(processor.supported_num_lookahead_tokens)}")
    del model


def fetch_mt() -> None:
    from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

    print(f"[2/2] {MT_MODEL}")
    tokenizer = AutoTokenizer.from_pretrained(MT_MODEL)
    model = AutoModelForSeq2SeqLM.from_pretrained(MT_MODEL)
    params = sum(p.numel() for p in model.parameters())
    print(f"      {params / 1e6:.0f}M parametros")
    # Comprobar un par de codigos: si el tokenizer llegara incompleto, esto
    # devolveria el token unk y las traducciones saldrian en otro idioma.
    for code in ("spa_Latn", "eng_Latn"):
        token_id = tokenizer.convert_tokens_to_ids(code)
        if token_id is None or token_id == tokenizer.unk_token_id:
            raise SystemExit(f"el tokenizer de NLLB no reconoce {code}: descarga incompleta")
    print("      codigos de idioma verificados")
    del model


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--skip-translator",
        action="store_true",
        help="no bajar NLLB (ahorra unos 2,4 GB si no vas a traducir)",
    )
    args = ap.parse_args()

    home = os.environ.get("HF_HOME")
    print(f"cache de modelos: {home if home else 'por defecto (~/.cache/huggingface)'}")
    before = cache_size()

    started = time.perf_counter()
    fetch_asr()
    if args.skip_translator:
        print("[2/2] NLLB omitido (--skip-translator)")
    else:
        fetch_mt()

    grew = cache_size() - before
    print(f"\nlisto en {time.perf_counter() - started:.0f}s")
    if grew > 0:
        print(f"descargado: {human(grew)}")
    else:
        print("ya estaba todo en cache")
    print(f"cache total: {human(cache_size())}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
