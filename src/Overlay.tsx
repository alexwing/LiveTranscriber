import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Entry, SessionEvent, Source, TranslatedLine } from "./tauri";

/** Cuantas lineas ya cerradas se mantienen en pantalla. */
const KEEP = 2;

/** Subtitulos sobre lo que estes viendo: ventana transparente, sin bordes. */
export default function Overlay() {
  const [recent, setRecent] = useState<string[]>([]);
  const [translated, setTranslated] = useState<string[]>([]);
  const [partials, setPartials] = useState<Partial<Record<Source, string>>>({});

  useEffect(() => {
    const unlisteners = [
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

  const live = [partials.system, partials.mic].filter(Boolean).join("  ");

  return (
    <div className="overlay" onMouseDown={() => getCurrentWindow().startDragging()}>
      <button
        className="overlay-close"
        onClick={(e) => {
          e.stopPropagation();
          getCurrentWindow().hide();
        }}
        title="Ocultar (se puede volver a abrir desde la bandeja)"
      >
        ×
      </button>
      <div className="overlay-text">
        {recent.map((line, i) => (
          <p key={i} className="overlay-old">
            {line}
          </p>
        ))}
        {live && <p className="overlay-live">{live}</p>}
        {/* La traduccion va debajo y en otro color: llega una frase por detras
            del original, asi que mezclarlas confundiria. */}
        {translated.map((line, i) => (
          <p key={`t${i}`} className="overlay-translated">
            {line}
          </p>
        ))}
        {!live && recent.length === 0 && <p className="overlay-idle">Esperando audio…</p>}
      </div>
    </div>
  );
}
