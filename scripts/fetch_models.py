"""Downloads and validates the models LiveTranscriber needs.

They are actually loaded instead of running a plain `snapshot_download`, for two
reasons:

1. `from_pretrained` downloads **only** the files transformers uses. The ASR
   repo also ships a 2.4 GB `.nemo` file we never touch, and downloading the
   whole repo would pull it in for nothing.
2. If the weights are corrupt or something is missing, it shows up here and not
   the first time the user hits Start.

It loads on CPU: no GPU is needed to provision, so the installer also works on a
machine where the card is not ready yet.

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
    print(f"      {params / 1e6:.0f}M parameters")
    print(f"      {len(processor.prompt_dictionary)} languages")
    print(f"      latencies: {sorted(processor.supported_num_lookahead_tokens)}")
    del model


def fetch_mt() -> None:
    from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

    print(f"[2/2] {MT_MODEL}")
    tokenizer = AutoTokenizer.from_pretrained(MT_MODEL)
    model = AutoModelForSeq2SeqLM.from_pretrained(MT_MODEL)
    params = sum(p.numel() for p in model.parameters())
    print(f"      {params / 1e6:.0f}M parameters")
    # Comprobar un par de codigos: si el tokenizer llegara incompleto, esto
    # devolveria el token unk y las traducciones saldrian en otro idioma.
    for code in ("spa_Latn", "eng_Latn"):
        token_id = tokenizer.convert_tokens_to_ids(code)
        if token_id is None or token_id == tokenizer.unk_token_id:
            raise SystemExit(f"the NLLB tokenizer does not recognize {code}: incomplete download")
    print("      language codes verified")
    del model


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--skip-translator",
        action="store_true",
        help="do not download NLLB (saves about 2.4 GB if you are not going to translate)",
    )
    args = ap.parse_args()

    home = os.environ.get("HF_HOME")
    print(f"model cache: {home if home else 'default (~/.cache/huggingface)'}")
    before = cache_size()

    started = time.perf_counter()
    fetch_asr()
    if args.skip_translator:
        print("[2/2] NLLB skipped (--skip-translator)")
    else:
        fetch_mt()

    grew = cache_size() - before
    print(f"\ndone in {time.perf_counter() - started:.0f}s")
    if grew > 0:
        print(f"downloaded: {human(grew)}")
    else:
        print("everything was already cached")
    print(f"total cache: {human(cache_size())}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
