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

export const SPLITS: Array<[Split, string, string]> = [
  ["combined", "≡", "Combinado: la traduccion debajo de cada parrafo"],
  ["split-v", "⬌", "Dividido vertical: uno al lado del otro"],
  ["split-h", "⬍", "Dividido horizontal: uno encima del otro"],
  ["only-original", "O", "Solo el original"],
  ["only-translated", "T", "Solo la traduccion"],
  ["meeting", "⊞", "Reunion: los demas arriba, yo abajo, un idioma por columna"],
];

/** Modos de vista que tienen sentido con esta configuracion.
 *
 *  Sin traduccion no hay nada que poner en la columna traducida, y la vista
 *  de reunion necesita las dos fuentes: con una sola, la mitad de sus cajas
 *  se quedaria vacia para siempre. Ofrecer modos que solo pueden salir en
 *  blanco es invitar a pensar que la app no funciona. */
export function availableSplits(cfg: AppConfig): Split[] {
  return SPLITS.map(([id]) => id).filter((id) => {
    if (id === "only-original") return true;
    if (!cfg.translate) return false;
    if (id === "meeting") return cfg.capture_system && cfg.capture_mic;
    return true;
  });
}

/** Nombre legible de un idioma a partir de su locale. */
export function languageName(code: string): string {
  const found = LANGUAGES.find(([c]) => c === code);
  return found ? found[1] : code;
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

/** Idiomas listos para transcribir, segun el model card. */
export const LANGUAGES: Array<[string, string]> = [
  ["auto", "Detectar automaticamente"],
  ["es-ES", "Espanol (Espana)"],
  ["es-US", "Espanol (America)"],
  ["en-US", "Ingles (EEUU)"],
  ["en-GB", "Ingles (Reino Unido)"],
  ["fr-FR", "Frances"],
  ["de-DE", "Aleman"],
  ["it-IT", "Italiano"],
  ["pt-BR", "Portugues (Brasil)"],
  ["pt-PT", "Portugues (Portugal)"],
  ["nl-NL", "Neerlandes"],
  ["tr-TR", "Turco"],
  ["ru-RU", "Ruso"],
  ["ar-AR", "Arabe"],
  ["hi-IN", "Hindi"],
  ["ja-JP", "Japones"],
  ["ko-KR", "Coreano"],
  ["vi-VN", "Vietnamita"],
  ["uk-UA", "Ucraniano"],
  ["zh-CN", "Chino mandarin"],
  ["pl-PL", "Polaco"],
];

/** Los unicos valores que acepta el modelo, con su latencia real. */
export const LOOKAHEADS: Array<[number, string]> = [
  [0, "80 ms - minima latencia, mas errores"],
  [3, "320 ms - equilibrio recomendado"],
  [6, "560 ms - mas preciso"],
  [13, "1120 ms - maxima precision"],
];

export function formatClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}
