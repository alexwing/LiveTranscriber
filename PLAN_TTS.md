# Plan: salida de voz con clonación (TTS)

Que lo que yo hablo en español salga por un micro virtual, en otro idioma, con mi
propia voz. Lo que dicen los demás se sigue **leyendo** en pantalla, como ahora.

Todo lo que se afirma abajo está **medido en esta máquina** (RTX 3060 12 GB,
Windows 11) contra el backend de voicebox en `E:\projects\voicebox`, no copiado de
documentación.

## Qué motor y por qué

Medido con un backend recién arrancado por motor (para que ninguno herede la VRAM
del anterior), texto de una frase (83 caracteres), `seed=1234`, 1 calentamiento y
3 medidas. Se da el mejor tiempo en caliente.

| motor | idioma | cold | warm | audio | RTFx | VRAM | idiomas | licencia |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Kokoro 82M | en | 6.723 ms | 115 ms | 5,35 s | 46,6x | 559 MB | 8 | Apache 2.0 |
| Kokoro 82M | es | 5.815 ms | 111 ms | 5,30 s | 47,9x | 557 MB | 8 | Apache 2.0 |
| **Chatterbox ML** | en | 21.180 ms | 4.435 ms | 3,74 s | 0,84x | 3.400 MB | **23** | **MIT** |
| **Chatterbox ML** | es | 21.694 ms | 4.565 ms | 4,24 s | 0,93x | 3.399 MB | **23** | **MIT** |
| **Chatterbox ML** | de | 20.940 ms | 4.606 ms | 4,46 s | 0,97x | 3.399 MB | **23** | **MIT** |
| Qwen 1.7B | en | 23.837 ms | 11.382 ms | 5,12 s | 0,45x | 4.046 MB | 10 | — |
| Qwen 1.7B | es | 23.038 ms | 12.618 ms | 5,68 s | 0,45x | 4.046 MB | 10 | — |

**Con clonación → Chatterbox Multilingual.** Gana a Qwen en las tres dimensiones a
la vez: 2,7x más rápido, más ligero y con más del doble de idiomas. Y cubre
alemán, ruso y coreano, que a Kokoro le faltan — que es lo que hace viable poder
cambiar de idioma destino más adelante.

**Sin clonación → Kokoro 82M.** 111 ms y 47,9x. Queda como modo alternativo
(voz neutra) y como red de seguridad si Chatterbox no da el ritmo.

LuxTTS y Chatterbox Turbo quedaron descartados: son **solo inglés**, y eso choca
con el requisito de cambiar de idioma. LuxTTS era el más rápido con clonación
(301 ms, 13x), así que si alguna vez el inglés se congela como único destino,
merece revisarlo.

## El dato que condiciona el diseño: RTFx > 1

Para habla sostenida lo que decide no es la latencia, es si RTFx supera 1. Por
debajo, generas más despacio de lo que se reproduce y **el retardo crece sin
estabilizarse** mientras sigas hablando. Por encima, el desfase queda acotado a
una frase.

Chatterbox está justo en el filo (0,84–0,97x por frase corta), pero con texto
largo (330 caracteres) sube a **1,02–1,03x**. Ajustando el coste sobre el par de
inglés (3,74 s y 15,9 s de audio generado):

- **coste fijo por llamada ≈ 1 s**
- **RTFx marginal ≈ 1,09x**

El ajuste predice el alemán largo con 0,1 s de error (17,2 s frente a 17,1 s
medidos) y falla ~0,5 s en los cortos, así que es aproximado. Pero deja claro
dónde está el problema: **el 0,84x de las frases cortas es casi todo ese segundo
fijo.**

### De dónde sale ese segundo, y cómo quitarlo

En voicebox, `chatterbox_backend.py` guarda el prompt de voz como una simple ruta
y devuelve `False` (no cacheado), y luego pasa `audio_prompt_path=ref_audio` a
`model.generate()` **en cada llamada**. Chatterbox re-codifica el audio de
referencia en cada frase.

En nuestro sidecar eso es evitable: llamar a `prepare_conditionals()` **una vez al
arrancar** y después `generate(text)` sin `audio_prompt_path`. Si elimina el
coste fijo, una frase corta pasaría de 0,84x a ~1,09x, sostenible incluso por
frase suelta.

**Esto es una hipótesis derivada de mis medidas, no algo verificado.** Medirlo es
la primera tarea de la fase 2, y si no se cumple, la mitigación es el agrupador de
la fase 3.

## Arquitectura

Misma forma que ya usamos: la lógica en crates independientes de Tauri, y el
motor detrás de un trait para poder cambiarlo sin que nada más se entere.

```
crates/asr-core/src/tts.rs           trait TtsEngine + TtsEvent + TtsError   (espejo de engine.rs)
crates/asr-core/src/tts_sidecar.rs   PythonTtsSidecar                        (espejo de sidecar.rs)
crates/asr-core/src/speech_out.rs    agrupador + cola ordenada + reproducción
crates/asr-audio/src/render.rs       salida WASAPI a un dispositivo concreto  (NUEVO)
sidecar/tts_server.py                el motor (Chatterbox | Kokoro)          (espejo de mt_server.py)
src-tauri/src/lib.rs                 comandos y eventos
```

`asr-audio` hoy **solo captura**. Sacar audio a un dispositivo elegido es
capacidad nueva, y es la única pieza sin precedente en el repo.

### Protocolo del sidecar

Mismo marco que `mt_server.py`: `u32 longitud | u8 tipo | payload`, tipo `0x02`
control JSON, una línea JSON por respuesta en stdout, y el `id` viajando de vuelta
para emparejar sin asumir orden.

```
stdin  (tipo 0x02, JSON utf-8)
    {"cmd":"speak","id":12,"text":"...","lang":"en","voice":"clone"}
    {"cmd":"shutdown"}

stdout (una línea JSON por mensaje)
    {"t":"ready","device":"cuda","engine":"chatterbox","dtype":"float16","rate":24000}
    {"t":"audio","id":12,"pcm":"<base64 i16 LE mono>","rate":24000,"ms":4560}
    {"t":"error","id":12,"message":"..."}
```

La única diferencia real con traducción es que la respuesta es audio. Va en
**base64 de PCM i16** dentro de la línea JSON, en vez de bytes crudos, para no
romper el protocolo de líneas y poder reutilizar el lector de `sidecar.rs`. Coste:
una frase de 4 s a 24 kHz i16 son 192 KB, 256 KB en base64 — despreciable al lado
de los 4,5 s que tarda en generarse. Si algún día molesta, se añade un tipo de
frame binario en stdout.

`pick_dtype` se copia tal cual de `mt_server.py`: el problema de bfloat16 emulado
en Turing es el mismo aquí.

### Dónde engancha en el pipeline

`SentenceSplitter::push` (`translate.rs`) ya devuelve frases cerradas, y
`TranslatedSentence` ya lleva su `paragraph`. **El TTS se engancha al evento de
frase traducida, no al cierre de párrafo** — es exactamente la lección que ya
está documentada en el README para la traducción, y aquí el coste de equivocarse
es peor: los demás esperarían `paragraph_idle_secs` enteros antes de oírte.

Flujo completo de salida:

```
micro → gate → ASR → SentenceSplitter → NLLB (es→destino) → agrupador → TTS → cola ordenada → render → CABLE Input
```

### El agrupador

Consecuencia directa del coste fijo por llamada. Acumula frases traducidas y
suelta el bloque cuando se cumple lo primero de:

- **N caracteres** acumulados (~250–300, donde ya se midió >1x), o
- **T ms** desde la primera frase pendiente (para que una frase suelta no se
  quede esperando).

Ambos configurables. Así se consigue RTFx > 1 sin latencia sin cota. Si la
hipótesis de `prepare_conditionals` se confirma, N puede bajar mucho o el
agrupador quedarse en `N=1`.

### Orden de reproducción

Las frases deben sonar en orden aunque la generación termine desordenada. El `id`
ya vuelve del sidecar, así que un búfer de reordenación indexado por `id` resuelve
el caso.

## Micro virtual

**En Windows no se puede crear un dispositivo de entrada de audio desde código de
usuario.** Hace falta un driver de kernel firmado; no se resuelve desde Rust.

La vía práctica es **VB-CABLE**: instala un par de dispositivos, `CABLE Input`
(reproducción) y `CABLE Output` (grabación). Nosotros renderizamos a `CABLE
Input`; en Teams el usuario elige `CABLE Output` como micrófono.

Tres consecuencias:

1. **Elimina el riesgo de realimentación.** El TTS nunca toca los altavoces, así
   que la captura por loopback no lo recoge. No hace falta el filtrado por PID
   para esto.
2. **Conviene fijar el formato del cable a 24 kHz mono**, que es lo que sacan
   Chatterbox y Kokoro, para que Windows no remuestree por su cuenta.
3. **Lo instala el usuario aparte** y tiene su propia licencia. El instalador debe
   detectar su ausencia y decirlo claro, no fallar de forma opaca.

## Ciclo de vida del proceso: el Job Object

Midiendo esto apareció un proceso huérfano de `multiprocessing.spawn` que
sobrevivió a su backend **reteniendo 11,6 GB de VRAM**: matarlo bajó la GPU de
12.045 a 447 MiB. Usaba además el Python del sistema, no el del venv, así que no
aparece donde lo buscarías.

Un `taskkill` al PID padre **no basta**. El sidecar debe lanzarse dentro de un
**Job Object** con `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (`CreateJobObjectW` +
`SetInformationJobObject` + `AssignProcessToJobObject`), que mata el árbol
completo incluidos los hijos de `multiprocessing`. Sin eso, un cierre sucio deja
la tarjeta inutilizable hasta reiniciar.

Aplica igual al sidecar de ASR y al de traducción que ya existen.

## Presupuesto de VRAM

| componente | VRAM |
|---|---:|
| ASR Nemotron | 2,40 GB |
| NLLB-200 | 1,27 GB |
| Chatterbox ML | 3,40 GB |
| Kokoro (si se carga a la vez) | 0,56 GB |
| escritorio | ~0,87 GB |
| **total con Chatterbox** | **~7,9 GB** |
| **total con las dos** | **~8,5 GB** |

Cabe en los 12 GB con margen. Se pueden tener las dos vías cargadas sin descargar
y recargar modelos.

## Arranque en frío

Chatterbox tarda **21 s** en cargar. Hay que precargarlo al iniciar la app, detrás
de un estado visible de "preparando voz", nunca en la primera frase. Kokoro son
~6 s.

## Fases

Cada fase termina en algo verificable sin la siguiente.

**Fase 0 — sondeo, sin escribir sidecar.** Validar calidad y latencia llamando al
backend de voicebox por HTTP (`POST /generate/stream`, puerto 17493). Barato y
responde la única pregunta que ningún número resuelve: si la voz clonada convence.
*Hecho en parte: hay muestras generadas en español, inglés y alemán.*

**Fase 1 — `asr-audio::render`.** Salida WASAPI a un dispositivo por nombre, más
un comando de `asr-cli` que reproduzca un `.wav` en él. Verificable sin ningún
modelo: si suena en `CABLE Input` y Teams lo oye por `CABLE Output`, la fase está
cerrada.

**Fase 2 — `tts_server.py` + `PythonTtsSidecar`.** Primero medir la hipótesis de
`prepare_conditionals` (una sola vez al arrancar frente a por llamada) y decidir
si el agrupador es necesario. Comando `asr-cli speak --text ... --lang en`.

**Fase 3 — cableado del pipeline.** Agrupador, cola ordenada y enganche al evento
de frase traducida en `session.rs`. Verificable con el CLI de punta a punta:
hablar al micro y que salga por el cable en otro idioma.

**Fase 4 — Tauri y interfaz.** Comandos y eventos; selector de motor
(Chatterbox/Kokoro), de voz, de dispositivo de salida, e interruptor. Mostrar
**profundidad de la cola**, que es la señal de que te estás quedando atrás.

**Fase 5 — instalador.** Dependencias del nuevo sidecar en `install.ps1`,
precomprobación de modelos, y detección de VB-CABLE con mensaje claro si falta.
Cuidado con lo ya aprendido: `$ErrorActionPreference = "Stop"` mata estos scripts,
y hay que declarar el sidecar nuevo como recurso en `tauri.conf.json` o el `.msi`
saldrá sin él.

## Riesgos

- **RTFx marginal.** 0,84–1,03x deja poco margen. Si en uso real se queda corto,
  las salidas son el agrupador, `prepare_conditionals`, o caer a Kokoro avisando.
- **Esto es interpretación con retardo, no simultánea.** Con la cascada completa
  pasan varios segundos desde que cierras una frase hasta que te oyen. La interfaz
  debe hacerlo evidente en vez de parecer que se ha colgado.
- **Todo el audio de Chatterbox lleva la marca de agua Perth de Resemble AI**,
  imperceptible pero presente. No es un impedimento; conviene saberlo.
- **NLLB sigue siendo el único bloqueo comercial** (CC-BY-NC-4.0). El TTS elegido
  no añade ninguno: Chatterbox es MIT y Kokoro Apache 2.0.
- **Errores en cascada.** Si el ASR oye mal, la traducción propaga y ahora además
  se pronuncia con tu voz. El listado en pantalla de lo que se ha dicho en tu
  nombre pasa a ser una función, no un lujo.

## Sin verificar

- La **calidad** de la voz clonada. Hay muestras generadas; es una decisión tuya,
  no medible.
- Chatterbox **con el ASR corriendo a la vez**. Todas mis medidas son del TTS
  solo; la contienda por GPU podría empeorarlas.
- La hipótesis de `prepare_conditionals`.
- **TADA 3B** (10 idiomas, con clonación) quedó sin medir: son 8 GB y Chatterbox
  ya cubre 23 idiomas.
