# LiveTranscriber

Transcribe en vivo **lo que suena en tu PC** — Teams, una película, el navegador —
y opcionalmente tu micrófono a la vez, en local y sin API de pago.

Usa [`nvidia/nemotron-3.5-asr-streaming-0.6b`](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b)
a través del proyecto hermano en `E:\projects\nemotron-3.5-asr-streaming-0.6b`.

Todo lo que se afirma abajo está medido en esta máquina (RTX 3060 12 GB, Windows 11),
no copiado de documentación.

## Cómo está montado

Misma forma que TapoController: workspace de Cargo con la lógica en crates
independientes de Tauri, y `src-tauri` como capa fina que solo traduce a la interfaz.

```
crates/asr-audio    captura WASAPI, normalización y gate      (≈ tapo-proto)
crates/asr-core     motor, sesión, historial, configuración
crates/asr-cli      banco de pruebas sin interfaz             (≈ tapo-cli)
src-tauri           comandos, bandeja, atajos, eventos
sidecar/            el proceso Python que corre el modelo
src/                React 18 + Vite + TS, wrapper tipado sobre invoke
```

El flujo: WASAPI → normalizador → gate de silencio → frames por stdin al sidecar →
el modelo devuelve texto por stdout → `emit` a la ventana → React lo pinta.

A diferencia de TapoController, aquí sí se usan eventos (`emit`/`listen`): el texto
en streaming necesita push, no vale con `invoke`.

### El motor está desacoplado a propósito

`asr-core::AsrEngine` es un trait. Hoy solo lo implementa `PythonSidecar`, que arrastra
un venv de PyTorch de ~5 GB. Cambiarlo por un motor ONNX en Rust puro es implementar
el trait otra vez y cambiar la línea donde se construye: ni la captura ni la interfaz
se enteran.

## Puesta en marcha

En una máquina nueva, un solo comando. Ver [INSTALL.md](INSTALL.md) para el detalle.

```bash
cd E:\projects\LiveTranscriber; .\scripts\install.ps1
```

Provisiona el entorno de Python, descarga los modelos, elige la precisión según la
GPU que encuentre, escribe la configuración con rutas absolutas, compila la app y
**comprueba que arranca de verdad** — lanza el sidecar, carga el modelo en la GPU y
le habla el protocolo real.

Si algo deja de ir después:

```bash
cd E:\projects\LiveTranscriber; .\scripts\verify.ps1
```

Y para el día a día:

```bash
cd E:\projects\LiveTranscriber; npm run app:dev
```

## Probar sin interfaz

`asr-cli` existe para diagnosticar sin levantar la GUI ni cargar el modelo.

```bash
cd E:\projects\LiveTranscriber; cargo run -p asr-cli -- devices
```

```bash
cd E:\projects\LiveTranscriber; cargo run -p asr-cli -- level --from system --seconds 10
```

```bash
cd E:\projects\LiveTranscriber; cargo run -p asr-cli -- run --from system --seconds 30 --language es-ES
```

```bash
cd E:\projects\LiveTranscriber; cargo run -p asr-cli -- run --from system --seconds 40 --language es-ES --translate-to en-US
```

`level` es el primero que hay que mirar si algo no va: dice si entra audio, a qué
nivel, y cuánta ganancia hace falta.

## Tres cosas que se descubrieron midiendo

Ninguna es obvia y las tres condicionan el diseño.

### 1. El loopback captura *después* del control de volumen

Un tono de origen con rms **0,6364** se capturó por loopback a rms **0,00252**:
**48 dB de atenuación**, que es exactamente lo que tenía el deslizador de Windows.

Consecuencia: con el volumen bajo, al modelo le llega prácticamente silencio y no
transcribe nada — sin ningún error que lo explique. Por eso hay un normalizador de
ganancia (`asr-audio::Normalizer`) activado por defecto, que sigue el pico reciente
y reescala. Cuando se queda sin margen, la interfaz avisa de que subas el volumen.

Y sí: **si silencias Windows, no se transcribe nada.** No hay forma de evitarlo por
esta vía.

### 2. El nivel de audio no sirve para decidir donde acaba un párrafo

Este fue el error de diseño más caro, y tuvo dos vidas.

Primero se cortaban los párrafos con un umbral **absoluto** en dBFS. Mal: el habla
por loopback llegaba a −62 dBFS (por lo del punto anterior), así que un umbral de
−50 la descartaba entera. Y con el normalizador compensando hasta ×64, el ruido de
fondo subía por encima de cualquier umbral fijo y el gate no volvía a cerrar nunca.

Se cambió a un umbral **relativo** al habla reciente —entre voz y pausa hay 20-30 dB
sin importar el volumen del sistema— y eso arregló el caso de una habitación silenciosa.
Pero seguía roto en el caso real: **con música de fondo el nivel nunca baja**, así que
el párrafo no cerraba jamás. Y como la traducción se disparaba al cerrar, tampoco
llegaba nunca. Se veía como "la traducción tarda muchísimo".

La señal correcta no es el audio: **es el propio reconocedor**. Si suena música pero
nadie habla, el modelo no emite texto. Así que un párrafo se cierra cuando el modelo
lleva `paragraph_idle_secs` sin transcribir nada nuevo, con un tope de
`paragraph_max_secs` para los monólogos sin pausas.

Verificado con música sonando sin interrupción y tres intervenciones habladas encima:
cuatro párrafos, cada uno cerrado y traducido.

Al gate de audio le queda solo decidir si merece la pena gastar GPU en un bloque. Ya
no decide párrafos.

### 3. Un umbral de silencio absoluto no vale ni para eso

La primera versión del gate comparaba con un umbral fijo en dBFS. Estaba mal, y dos
medidas lo demostraron:

- El habla capturada por loopback llegaba a rms 0,0005 (**−62 dBFS**), por lo del punto
  anterior. Un umbral de −50 dBFS la descartaba entera.
- Con el normalizador compensando hasta ×64, el ruido de fondo sube por encima de
  cualquier umbral fijo y el gate **no vuelve a cerrar nunca**: toda la sesión queda
  como un único párrafo interminable, sin saltos de línea.

Hablar y callarse es una diferencia **relativa** —entre voz y pausa hay 20-30 dB— sin
importar a qué volumen esté el sistema. Así que el gate sigue el nivel del habla
reciente (ataque inmediato, caída lenta) y considera pausa lo que caiga `gate_drop_db`
por debajo. El umbral absoluto se queda solo como suelo, a −80 dBFS.

El gate decide con el nivel **crudo**, antes de normalizar, que es la medida que de
verdad distingue voz de pausa.

### 4. Un dispositivo de salida ocioso no genera ni un evento

Capturando por loopback un dispositivo por el que no sonaba nada llegaron **cero
bloques**, no bloques de silencio. WASAPI simplemente no dispara el evento.

Por eso el bucle de captura trata `EventTimeout` como situación normal y no como
error. Sin eso, la app fallaría al arrancar siempre que no hubiera nada sonando.

### 5. El ejemplo oficial de NVIDIA se rompe con lookahead 0

Heredado del proyecto del modelo: con `lookahead = 0` el chunk inicial cubre 1 solo
frame mel, así que el primer chunk del bucle pide la muestra `1×160 − 256 = −96`.
NumPy lee ese negativo como índice desde el final y devuelve un slice vacío, y el
STFT revienta. Aquí se rellena con silencio, igual que en el proyecto del modelo.

### Y tres del instalador, que solo salieron probando en limpio

Los tres eran invisibles en la máquina de desarrollo. Salieron al ejecutar el
instalador sobre una copia del proyecto sin `.venv` ni configuración.

**`librosa` sí hace falta, aunque el audio lo capture Rust.** Deduje la lista de
dependencias leyendo los `import` de los sidecars y me salió corta: `transformers`,
`numpy` y `huggingface_hub`. Pero `NemotronAsrStreamingFeatureExtractor` declara
`librosa` como *backend obligatorio*, así que `AutoProcessor.from_pretrained` falla
con `ImportError` antes de mirar ningún audio. En el entorno de desarrollo estaba ya
instalada de pruebas anteriores, así que nunca dio la cara.

**`$ErrorActionPreference = "Stop"` mata los scripts de PowerShell.** Cualquier cosa
que un `.exe` escriba en stderr se convierte en error *terminante*, y `2>$null` no lo
evita: solo esconde el texto. Una sonda tan inocente como "¿está torch instalado?"
abortaba el instalador con el traceback de Python, y un simple aviso de `pip` habría
hecho lo mismo a mitad de instalación. Todas las llamadas nativas pasan ahora por un
envoltorio que baja la preferencia a `Continue` y decide por el código de salida.

**El `.msi` no llevaba el sidecar de traducción.** `tauri.conf.json` declaraba como
recurso solo `asr_server.py`, así que una instalación con el MSI habría transcrito
pero fallado al traducir. Y el bundle no sale en `src-tauri\target\release\bundle`
sino en `target\release\bundle`, porque al ser un workspace de Cargo el directorio
`target` está en la raíz.

### Y dos más de la capa Tauri

**COM es por hilo, y el hilo de Tauri está en STA.** WebView2 deja el hilo desde el
que Tauri atiende los comandos en STA, así que `initialize_mta()` falla allí con
`RPC_E_CHANGED_MODE` (0x80010106) y la lista de dispositivos se quedaba vacía con un
error en rojo. `list_devices` enumera ahora en un hilo propio, donde siempre hay un
MTA limpio.

Los nombres se forman como `AAAA_MM_DD_<nombre>.<ext>`, con el nombre base
configurable y sufijo por formato (`_traducida`, `_bilingue`). Si ya existe uno igual
ese día se añade `_2`, `_3`… en vez de sobrescribir: dos exportaciones del mismo día
con el mismo nombre son normales, y perder la primera en silencio no lo es.

**Las transcripciones acabaron donde nadie las buscaba.** Consecuencia del mismo
problema: `output_dir` valía `"."`, que se resuelve contra el directorio de trabajo del
proceso. Con `tauri dev` eso es `src-tauri/`, así que ahí aparecieron los `.txt` y
`.srt` sin que nadie lo pidiera. Ahora la carpeta se elige con un selector
(`tauri-plugin-dialog`), se guarda absoluta en la configuración, y una ruta relativa en
el TOML se ignora en favor de `Documentos\LiveTranscriber`. La interfaz muestra la ruta
efectiva para que nunca haya duda, y el comando de exportación recibe solo el nombre
del fichero: la carpeta la decide Rust.

**El directorio de trabajo no es de fiar.** `tauri dev` lanza el binario desde
`src-tauri/`, no desde la raíz, así que una ruta relativa como
`sidecar/asr_server.py` se resolvía a `src-tauri/sidecar/asr_server.py` y la app
decía *no encuentro el sidecar*. Un `.exe` instalado o un acceso directo tienen
todavía otro cwd. Ahora las rutas relativas se buscan en varias bases (cwd, el
directorio del ejecutable subiendo niveles, y el directorio de recursos del bundle),
y si no aparece el error **lista dónde ha mirado**.

Ojo con el marcador de raíz: el sidecar está declarado como recurso del bundle, así
que Tauri lo copia también a `target/debug/sidecar/` y no sirve para distinguir la
raíz de verdad. Para eso se usa `transcriber-config.example.toml`, que no se copia.

**Vite puede escuchar solo en IPv6.** Sin `server.host` explícito, Vite se ató a
`::1` mientras el `devUrl` de Tauri apunta a `127.0.0.1`: la ventana mostraba un
`ERR_CONNECTION_REFUSED` del navegador en vez de la interfaz. `vite.config.ts` fija
ahora `host: "127.0.0.1"`.

Los dos son invisibles si solo compruebas que la app "arranca": el proceso vive y la
ventana tiene el título correcto en ambos casos. Hizo falta mirar una captura.

## Traducción

**El modelo de voz no traduce.** Se comprobó de dos formas: el model card no lo
menciona en ningún sitio (su parámetro `target_lang` es el idioma *de origen*, pese
al nombre), y el propio modelo lo confirma — sus 121 prompts son todos locales y su
vocabulario de 13.089 tokens no tiene ni un token de tarea. No hay por dónde pedírselo.

Así que la traducción es un segundo paso encadenado, con
[NLLB-200-distilled-600M](https://huggingface.co/facebook/nllb-200-distilled-600M)
en su propio sidecar. Uno solo da servicio a todas las fuentes.

**Se traduce frase a frase, y se muestra agrupado por párrafos.** Esa combinación sale
de dos intentos fallidos:

1. Traducir por frases **y mostrarlas por frases**: quedaba troceado y no encajaba con
   la transcripción, que va por párrafos.
2. Esperar a que el párrafo cerrara y traducirlo entero: dos problemas a la vez.
   **NLLB se comía contenido** —de *"La primera parte consiste en capturar el audio del
   sistema. Eso ya funciona bien."* solo devolvía la primera frase, porque está
   entrenado a nivel de frase— y sobre todo la traducción **tardaba una eternidad**,
   porque no salía nada hasta el cierre del párrafo.

Lo que hay ahora: cada frase se traduce en cuanto su puntuación la cierra (~160 ms) y
se etiqueta con el párrafo al que pertenece. La interfaz junta las que comparten
etiqueta y las pinta como un bloque. Latencia de una frase, presentación por párrafos,
y ninguna frase perdida.

Medido aquí: **~160 ms por frase** y **1,27 GB de VRAM**. Con el ASR son ~3,7 GB de
los 12 de la tarjeta.

**Limitación heredada de NLLB:** necesita saber el idioma de origen, así que para
traducir hay que elegir un idioma concreto en vez de *Detectar automáticamente*. La
app lo avisa en vez de traducir desde un idioma equivocado.

**Licencia:** NLLB-200 es **CC-BY-NC-4.0, uso no comercial**. Para uso personal no hay
problema; para un producto habría que cambiarlo por Opus-MT o MADLAD-400, que es
implementar `Translator` otra vez y nada más.

Es una cascada, con lo que eso implica: si el reconocimiento oye mal una palabra, la
traducción propaga el error. No es interpretación simultánea profesional, es
subtitulado traducido con una frase de retardo.

## Portabilidad

### Otra GPU NVIDIA en Windows

Funciona sin tocar nada, con un matiz de precisión. El model card lista como
soportadas *"NVIDIA Ampere, NVIDIA Blackwell, NVIDIA Hopper, NVIDIA Jetson, NVIDIA
Lovelace, NVIDIA Turing, NVIDIA Volta"*, así que el modelo va también en Turing (RTX
20xx) y Volta.

Pero **bfloat16 necesita Ampere o superior** (capability 8.0+). En una Turing PyTorch
no falla: lo *emula*, y la app va lentísima sin ninguna pista de por qué —
`is_bf16_supported()` devuelve `True` por emulación salvo que le pases
`including_emulation=False`. Los sidecars detectan la capability y bajan a float16
avisando por el log.

El coste es real: medido en este modelo, float16 transcribe peor (mete muletillas
inexistentes) y va más lento que bf16 (RTFx 8,7 frente a 15,7). Con float32 no hay
pérdida de calidad pero sube la VRAM y baja el ritmo.

**Varias GPU no aportan nada tal como está.** Cada sidecar coge un solo dispositivo
(`cuda`, es decir `cuda:0`), así que una segunda tarjeta se quedaría parada.
Repartirlas sería un cambio pequeño —pasarle `--device cuda:1` a un sidecar— pero no
merece la pena: una sola tarjeta ya aguanta ~4 flujos a 320 ms y aquí se usan uno o dos.
La GPU no es el cuello de botella.

### macOS / Apple Silicon

**Hoy no funciona**, y son dos problemas independientes.

**La captura es Windows-only por construcción.** WASAPI, con 17 `cfg(windows)` en
`asr-audio`. En macOS el crate compila pero `list_devices` y `spawn_capture` devuelven
`UnsupportedPlatform`: la app arrancaría y no capturaría nada. Todo lo de arriba
—`asr-core`, Tauri, React— es agnóstico de plataforma, así que el trabajo se concentra
en un backend nuevo detrás de la misma API.

La buena noticia es que ya no hace falta un dispositivo virtual tipo BlackHole:
macOS 14.4 añadió los *Core Audio Process Taps*, que permiten capturar el audio del
sistema con permiso del usuario, y [cpal](https://github.com/RustAudio/cpal/releases)
trae loopback por CoreAudio para macOS > 14.6.

**Lo arriesgado no es el port, es el modelo.** El model card solo lista *"Linux, Linux 4
Tegra"* como sistema operativo, y solo arquitecturas CUDA. En un Mac habría que ir por
MPS, y ahí los riesgos concretos son el decodificador LSTM del RNNT y la cobertura de
bfloat16. El respaldo sería CPU, y no tengo ninguna medida de RTFx en CPU ni en M1: las
cifras de este README son de una 3060.

Antes de escribir una línea de captura para macOS, lo sensato es probar el modelo solo
en el Mac con un script de veinte líneas. Si en MPS no da tiempo real, el port de audio
sobra.

## Rendimiento medido (RTX 3060)

| lookahead | latencia | RTFx | flujos simultáneos |
|---|---|---|---|
| 0 | 80 ms | 1,8x | ~1 |
| 3 (defecto) | 320 ms | 4,6x | ~4 |
| 6 | 560 ms | 6,3x | ~6 |
| 13 | 1120 ms | 9,4x | ~9 |

Capturar sistema **y** micro a la vez son dos sesiones, cada una con su proceso
Python y su copia del modelo en VRAM (~2,4 GB cada una). A lookahead 3 la 3060 lo
lleva de sobra.

## Detalles de diseño

**El gate no descarta los silencios cortos.** El modelo es cache-aware y su estado
asume audio contiguo; quitar trozos produciría basura en las uniones. Dentro de un
segmento pasa todo, y solo tras un silencio largo (2 s por defecto) se cierra el
segmento y se reinicia el modelo. Eso además da los puntos de corte naturales para
las líneas del `.srt`.

**Loopback por proceso.** `CaptureTarget::Process { pid }` usa
`ActivateAudioInterfaceAsync` para capturar solo el audio de un proceso concreto —
Teams sin que se cuele la música. Está implementado en el crate y expuesto en el CLI
(`--pid`), pero todavía no en la interfaz.

**Cerrar la ventana no cierra la app**, la manda a la bandeja, como TapoController.

## Estado

Verificado de punta a punta con el CLI: captura loopback real, normalización, gate,
protocolo con el sidecar, transcripción y exportación a `.txt` y `.srt`. La prueba
grabó dos frases separadas por una pausa y el gate las partió en dos líneas
correctas, con sus tiempos.

18 tests unitarios en `cargo test --workspace` (gate, normalizador, transcripción,
configuración).

**Sin verificar todavía:** la interfaz gráfica en ejecución (ventana, bandeja,
atajos globales y overlay). El código compila y los comandos están cableados, pero
no se ha hecho una pasada visual.



## rediseño

Cuando dije de dividir las ventanas solo me referia a transcripción y traducción. Osea tener una pestaña con la configuración y otra con la transcripción y traducción, y esta ultima es la que se puede cambiar a dividido o combinado.

