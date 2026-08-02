import { invoke } from "@tauri-apps/api/core";

export type Source = "system" | "mic";
export type DeviceKind = "output" | "input";

export interface AppConfig {
  python: string;
  script: string;
  mt_script: string;
  translate: boolean;
  target_language: string;
  language: string;
  lookahead: number;
  dtype: string;
  capture_system: boolean;
  capture_mic: boolean;
  system_device_id: string | null;
  mic_device_id: string | null;
  /** Idioma del microfono. "" = el mismo al que se traduce la sala. */
  mic_language: string;
  /** A que se traduce el micro (= lo que pronuncia la voz). "" = el de la sala. */
  mic_target_language: string;
  gate_drop_db: number;
  gate_floor_dbfs: number;
  gate_hold_secs: number;
  paragraph_idle_secs: number;
  paragraph_max_secs: number;
  normalize_gain: boolean;
  hotkey_toggle: string;
  hotkey_overlay: string;
  overlay_enabled: boolean;
  output_dir: string;
  output_name: string;
  speak: SpeakConfig;
}

/** Voz sintetica: hablar la traduccion de lo que dices por el microfono.
 *  Funcion opcional con su propia seccion; apagada no cuesta nada. */
export interface SpeakConfig {
  enabled: boolean;
  engine: "chatterbox" | "kokoro";
  python: string;
  script: string;
  voice_wav: string | null;
  kokoro_voice: string;
  output_device_id: string | null;
  group_max_chars: number;
  group_max_wait_ms: number;
  mark_echo: boolean;
}

export interface AudioDevice {
  id: string;
  name: string;
  kind: DeviceKind;
  is_default: boolean;
}

export interface Entry {
  source: Source;
  start_ms: number;
  end_ms: number;
  text: string;
}

/** Una frase traducida y su original. `paragraph` agrupa las del mismo
 *  parrafo: cada frase llega en cuanto esta lista, y la interfaz las junta.
 *  `echo` marca la propia voz sintetica volviendo por la captura del
 *  sistema: no se re-traduce, se pinta atenuada y con etiqueta. */
export interface TranslatedLine {
  source: Source;
  paragraph: number;
  at_ms: number;
  original: string;
  translated: string;
  echo?: boolean;
}

/** Junta las frases de cada parrafo en un solo bloque para pintarlo.
 *  Las frases eco no se funden con las normales aunque compartan parrafo:
 *  alguien puede hablar mientras tu propia voz sintetica vuelve, y mezclar
 *  ambas en un bloque haria ilegible quien dijo que. */
export function groupByParagraph(lines: TranslatedLine[]): TranslatedLine[] {
  const out: TranslatedLine[] = [];
  for (const line of lines) {
    const last = out[out.length - 1];
    if (
      last &&
      last.paragraph === line.paragraph &&
      last.source === line.source &&
      !!last.echo === !!line.echo
    ) {
      last.original = `${last.original} ${line.original}`;
      last.translated = `${last.translated} ${line.translated}`;
    } else {
      out.push({ ...line });
    }
  }
  return out;
}

export type ExportFormat = "txt" | "srt" | "translated-srt" | "bilingual";

/** Las dos pestanas de la ventana. */
export type Tab = "config" | "transcript";

/** Como repartir la pestana de transcripcion entre original y traduccion. */
export type Split =
  | "combined"
  | "split-v"
  | "split-h"
  | "only-original"
  | "only-translated"
  | "meeting";

/** Los modos y su glifo. El texto explicativo lo pone `i18n`, para que no
 *  haya cadenas visibles fuera del catalogo de idiomas. */
export const SPLITS: Array<[Split, string]> = [
  ["combined", "≡"],
  ["split-v", "⬌"],
  ["split-h", "⬍"],
  ["only-original", "O"],
  ["only-translated", "T"],
  ["meeting", "⊞"],
];

/** Modos de vista que tienen sentido con esta configuracion.
 *
 *  Sin traduccion no hay nada que poner en la columna traducida, y la vista
 *  de reunion necesita las dos fuentes: con una sola, la mitad de sus cajas
 *  se quedaria vacia para siempre. Ofrecer modos que solo pueden salir en
 *  blanco es invitar a pensar que la app no funciona. */
export function availableSplits(cfg: AppConfig): Split[] {
  return SPLITS.map(([id]) => id).filter((id: Split) => {
    if (id === "only-original") return true;
    if (!cfg.translate) return false;
    if (id === "meeting") return cfg.capture_system && cfg.capture_mic;
    return true;
  });
}


/** Eventos que llegan por `session-event`. */
export type SessionEvent =
  | { kind: "ready"; source: Source; device: string; latency_ms: number; language: string }
  | { kind: "delta"; source: Source; at_ms: number; text: string }
  | { kind: "segment_end"; source: Source; at_ms: number }
  | { kind: "level"; source: Source; rms: number; gain: number; gain_at_ceiling: boolean }
  | { kind: "error"; source: Source; message: string }
  | { kind: "stopped"; source: Source };

/** Eventos que llegan por `speech-event` (voz sintetica). `queued_ms` es el
 *  retraso de voz acumulado: si crece sin parar, se esta hablando mas rapido
 *  de lo que el sintetizador genera. */
export type SpeechEvent =
  | { kind: "ready"; device: string; rate: number }
  | { kind: "queue"; pending_texts: number; queued_ms: number }
  | { kind: "spoke"; text: string; synth_ms: number; audio_ms: number }
  | { kind: "error"; message: string }
  | { kind: "stopped" };

// Los errores de los comandos llegan como { message }.
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e: any) {
    throw new Error(e?.message ?? String(e));
  }
}

/** Un dispositivo del perfil que ya no existe y ha caido al predeterminado. */
export interface DeviceFallback {
  /** "sistema" | "microfono" | "voz" */
  what: string;
  missing_id: string;
}

export interface AppliedProfile {
  config: AppConfig;
  fallbacks: DeviceFallback[];
}

export const api = {
  getConfig: () => call<AppConfig>("get_config"),
  saveConfig: (cfg: AppConfig) => call<void>("save_config", { new: cfg }),
  listDevices: (kind: DeviceKind) => call<AudioDevice[]>("list_devices", { kind }),
  listProfiles: () => call<string[]>("list_profiles"),
  /** Guarda la configuracion actual con ese nombre. Repetir nombre actualiza. */
  saveProfile: (name: string) => call<string[]>("save_profile", { name }),
  /** Aplica el perfil y dice que dispositivos han caido al predeterminado. */
  loadProfile: (name: string) => call<AppliedProfile>("load_profile", { name }),
  deleteProfile: (name: string) => call<string[]>("delete_profile", { name }),
  isRunning: () => call<boolean>("is_running"),
  start: () => call<void>("start_transcription"),
  stop: () => call<void>("stop_transcription"),
  getTranscript: () => call<Entry[]>("get_transcript"),
  getTranslations: () => call<TranslatedLine[]>("get_translations"),
  transcriptAsText: (what: "original" | "translated" | "both") =>
    call<string>("transcript_as_text", { what }),
  clearTranscript: () => call<void>("clear_transcript"),
  /** Solo el formato: carpeta, nombre y fecha los pone la configuracion. */
  exportTranscript: (format: ExportFormat) =>
    call<string>("export_transcript", { format }),
  /** Como se llamaria el fichero si se exportara ahora. */
  outputFilenamePreview: (format: ExportFormat) =>
    call<string>("output_filename_preview", { format }),
  outputDir: () => call<string>("output_dir"),
  /** Abre el selector. `null` si el usuario cancela. */
  pickOutputDir: () => call<string | null>("pick_output_dir"),
  revealOutputDir: () => call<void>("reveal_output_dir"),
  toggleOverlay: () => call<boolean>("toggle_overlay"),
  /** El menu de la bandeja lo dibuja el sistema, no el webview: hay que
   *  decirle el idioma aparte. */
  setUiLanguage: (lang: string) => call<void>("set_ui_language", { lang }),
};

/** Copia al portapapeles con respaldo, porque el webview puede bloquear la
 *  API asincrona segun el contexto en que se sirva la pagina. */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    document.body.removeChild(area);
  }
}

/** Codigos de idioma listos para transcribir, segun el model card. Los
 *  nombres visibles estan en `i18n`, traducidos. */
export const LANGUAGES: string[] = [
  "auto",
  "es-ES",
  "es-US",
  "en-US",
  "en-GB",
  "fr-FR",
  "de-DE",
  "it-IT",
  "pt-BR",
  "pt-PT",
  "nl-NL",
  "tr-TR",
  "ru-RU",
  "ar-AR",
  "hi-IN",
  "ja-JP",
  "ko-KR",
  "vi-VN",
  "uk-UA",
  "zh-CN",
  "pl-PL",
];

/** Los unicos valores que acepta el modelo. La descripcion esta en `i18n`. */
export const LOOKAHEADS: number[] = [0, 3, 6, 13];

export function formatClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}
