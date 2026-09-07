import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { AppConfig, Entry, SessionEvent, Source, TranslatedLine, api } from "./tauri";
import { Lang, STRINGS, initialLang } from "./i18n";

/** Cuantas lineas ya cerradas se mantienen en pantalla. */
const KEEP = 2;

/** Que idioma se pinta: lo que se dice, su traduccion, o los dos. */
type Mode = "original" | "translated" | "both";
const MODE_KEY = "overlay-mode";

function initialMode(): Mode {
  try {
    const saved = localStorage.getItem(MODE_KEY);
    if (saved === "original" || saved === "translated" || saved === "both") return saved;
  } catch {
    // sin almacenamiento (vista previa, datos borrados): el valor por defecto
  }
  return "both";
}

/** "es-ES" -> "ES"; "auto" no tiene codigo corto util. */
function shortCode(code: string): string {
  if (!code || code === "auto") return "?";
  return code.split("-")[0].toUpperCase();
}

/** Subtitulos sobre lo que estes viendo: ventana transparente, sin bordes. */
export default function Overlay() {
  const [recent, setRecent] = useState<string[]>([]);
  const [translated, setTranslated] = useState<string[]>([]);
  const [partials, setPartials] = useState<Partial<Record<Source, string>>>({});
  const [mode, setMode] = useState<Mode>(initialMode);
  // La configuracion solo hace falta para etiquetar los idiomas del selector
  // y saber si hay traduccion. Se relee al arrancar una sesion, que es cuando
  // la ventana principal la ha podido cambiar.
  const [config, setConfig] = useState<AppConfig | null>(null);
  // Webview aparte: arranca con lo que haya guardado y luego escucha los
  // cambios que hace la ventana principal.
  const [lang, setLang] = useState<Lang>(initialLang);
  const t = STRINGS[lang];

  useEffect(() => {
    const refresh = () => api.getConfig().then(setConfig).catch(() => {});
    refresh();
    const unlisteners = [
      listen<Lang>("ui-lang", ({ payload }) => setLang(payload)),
      listen<boolean>("running-changed", ({ payload }) => {
        if (payload) refresh();
      }),
      listen<SessionEvent>("session-event", ({ payload }) => {
        if (payload.kind === "delta") {
          setPartials((prev) => ({
            ...prev,
            [payload.source]: (prev[payload.source] ?? "") + payload.text,
          }));
        } else if (payload.kind === "segment_end") {
          setPartials((prev) => ({ ...prev, [payload.source]: "" }));
        }
      }),
      listen<Entry>("transcript-entry", ({ payload }) => {
        setRecent((prev) => [...prev, payload.text].slice(-KEEP));
      }),
      listen<TranslatedLine>("translation", ({ payload }) => {
        setTranslated((prev) => [...prev, payload.translated].slice(-KEEP));
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((off) => off()));
    };
  }, []);

  const choose = (next: Mode) => {
    setMode(next);
    try {
      localStorage.setItem(MODE_KEY, next);
    } catch {
      // se pierde al cerrar; no pasa nada
    }
  };

  // Sin traduccion no hay nada que elegir: solo existe el original.
  const translating = config?.translate ?? false;
  const effective: Mode = translating ? mode : "original";
  const showOriginal = effective !== "translated";
  const showTranslated = translating && effective !== "original";
  const codeFrom = shortCode(config?.language ?? "");
  const codeTo = shortCode(config?.target_language ?? "");

  const live = [partials.system, partials.mic].filter(Boolean).join("  ");
  const empty =
    (!showOriginal || (!live && recent.length === 0)) &&
    (!showTranslated || translated.length === 0);

  return (
    <div className="overlay" onMouseDown={() => getCurrentWindow().startDragging()}>
      {/* El mousedown NO puede llegar al contenedor: arrancaria el arrastre de
          la ventana y el clic nunca se completaria. Era lo que dejaba la X sin
          efecto. */}
      <div className="overlay-controls" onMouseDown={(e) => e.stopPropagation()}>
        {translating && (
          <div className="overlay-modes" role="radiogroup">
            <button
              className={effective === "original" ? "active" : ""}
              onClick={() => choose("original")}
              title={t.overlayOnlyOriginal}
            >
              {codeFrom}
            </button>
            <button
              className={effective === "translated" ? "active" : ""}
              onClick={() => choose("translated")}
              title={t.overlayOnlyTranslated}
            >
              {codeTo}
            </button>
            <button
              className={effective === "both" ? "active" : ""}
              onClick={() => choose("both")}
              title={t.overlayBoth}
            >
              {codeFrom}+{codeTo}
            </button>
          </div>
        )}
        <button
          className="overlay-close"
          onClick={() => getCurrentWindow().hide()}
          title={t.overlayHide}
        >
          ×
        </button>
      </div>
      <div className="overlay-text">
        {showOriginal &&
          recent.map((line, i) => (
            <p key={i} className="overlay-old">
              {line}
            </p>
          ))}
        {showOriginal && live && <p className="overlay-live">{live}</p>}
        {/* La traduccion va debajo y en otro color: llega una frase por detras
            del original, asi que mezclarlas confundiria. Sola, en el modo de
            un idioma, hereda el tamano del texto en vivo para que se lea. */}
        {showTranslated &&
          translated.map((line, i) => (
            <p
              key={`t${i}`}
              className={showOriginal ? "overlay-translated" : "overlay-translated-solo"}
            >
              {line}
            </p>
          ))}
        {empty && <p className="overlay-idle">{t.overlayIdle}</p>}
      </div>
    </div>
  );
}
