# phonia -- fase 0

Spike de línea de comandos que valida, de punta a punta, el camino de audio de **phonia**
(el futuro reproductor TIDAL hi-fi con TUI + daemon): login PKCE -> `playbackinfo` HiRes ->
descarga de segmentos DASH -> decodificación FLAC -> salida ALSA *bit-perfect* a un DAC USB.

No hay TUI ni daemon todavía: esto es un CLI de una sola pasada para probar que cada eslabón
de la cadena funciona con hardware real (un Fosi Audio DS2 en `hw:1,0` durante el desarrollo).

## Comandos

- **`phonia login`** -- Inicia sesión en TIDAL con el flujo PKCE (obligatorio: el flujo de
  device-code nunca recibe la entitlement `HI_RES_LOSSLESS`, aunque la cuenta la tenga). Abre
  una URL en el navegador (o la imprime si no se pudo abrir sola); tras iniciar sesión, TIDAL
  redirige a una página de error ("oops"), eso es normal -- copia la URL completa de la barra
  de direcciones y pégala en la terminal. Guarda la sesión en
  `~/.config/phonia/session.json` (permisos `0600`).

- **`phonia play <TRACK_ID> [--device hw:1,0] [--quality hires|lossless] [--save-mp4 <ruta>]`**
  -- Descarga y reproduce una pista por su ID. Consulta `playbackinfo`, descarga el manifiesto
  DASH (HiRes) o el archivo directo (Lossless/High/Low), lo decodifica y lo saca por ALSA.
  `--save-mp4` además guarda los bytes descargados en disco (útil para inspeccionar el fMP4).

- **`phonia play-file <ruta> [--device hw:1,0]`** -- Decodifica y reproduce un archivo local
  (FLAC o fMP4) por la misma ruta de salida ALSA, sin tocar TIDAL. Sirve para probar el DAC de
  forma aislada.

- **`phonia probe-device [--device hw:1,0]`** -- Abre el dispositivo ALSA indicado en modo
  reproducción (sin escribir nada) y lista qué formatos (`S16_LE`, `S24_3LE`, `S24_LE`,
  `S32_LE`) y qué frecuencias (44.1 kHz .. 384 kHz) acepta de forma nativa.

## Cómo verificar que la salida es bit-perfect

Al reproducir con `phonia play` o `phonia play-file` contra un dispositivo `hw:N,D`, tras el
primer bloque de audio se imprime el contenido de
`/proc/asound/card<N>/pcm<D>p/sub0/hw_params` seguido de un veredicto:

```
Fuente: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE 96000 Hz  ✔ BIT-PERFECT
```

o, si algo no cuadra (frecuencia o formato distintos a los negociados, o el dispositivo no es
`hw:N,D`):

```
Fuente: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE  ✖ CONVERTED (la tarjeta reporta 48000 Hz en vez de 96000 Hz)
```

También puedes comprobarlo a mano en otra terminal mientras suena algo:

```sh
cat /proc/asound/card1/pcm0p/sub0/hw_params
```

Si ves `closed`, nada tiene el dispositivo abierto en ese momento.

## Importante: PipeWire no debe tener el DAC abierto

`phonia` abre el dispositivo ALSA (`hw:1,0` por defecto) directamente, sin pasar por
`plughw`/`default`/`dmix`, porque cualquiera de esas capas puede remuestrear o mezclar el
audio y rompe la garantía de bit-perfect. Si PipeWire (u otra aplicación) ya tiene el DAC
abierto, `phonia` fallará al abrir el dispositivo con un error EBUSY explicando que hay que
liberarlo primero (por ejemplo, pausando la reproducción hacia esa tarjeta desde PipeWire, o
silenciando/deshabilitando su perfil para esa tarjeta mientras se usa `phonia`).

## Estado de la fase 0

- `cargo build` y `cargo test` pasan limpio; `cargo clippy` sin warnings.
- Lo que sigue quedando fuera de esta fase (llegará en fases posteriores): streaming real sin
  buffering completo en memoria, TUI, daemon, gapless, `%0Nd` en plantillas de segmento DASH,
  soporte de cifrado de manifiesto.
