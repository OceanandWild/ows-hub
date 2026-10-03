# OWS Hub

Launcher desktop del ecosistema **Ocean & Wild** — Tauri v2 (Rust + WebView2).

> **Descargas obligatorias en el navegador.** El panel web (`owsdatabase/web/OWS`)
> bloquea la descarga de juegos si no detecta el Hub. Esta app es la única vía
> para descargar, instalar y jugar con 1 clic.

## Qué hace

| Comando | Efecto |
|---|---|
| `get_library_dir` | Crea y devuelve `%APPDATA%/OWS/library` (biblioteca de juegos) |
| `download_installer` | Baja el ZIP del juego desde el servidor con eventos de progreso |
| `extract_zip` | Extrae el ZIP en la biblioteca (protegido contra zip-slip) |
| `find_game_exe` | Localiza el `.exe` jugable real (ignora crash-handler, `_Data`, stockfish) |
| `launch_game` | Lanza el juego con `cwd` = carpeta del `.exe` |

## Estructura

```
src-tauri/            Backend Rust (comandos Tauri)
  tauri.conf.json     Producto, ventana, bundle NSIS/MSI, updater
  capabilities/       Permisos del WebView
scripts/
  sync-app.ps1        Copia el frontend del monorepo a ./app
  publish-release.ps1 Sube los artefactos + genera latest.json
app/                  Frontend embebido (generado, NO se versiona)
.github/workflows/    Release en tag (v*)
```

## Frontend: fuente única

El frontend **no vive en este repo**. Se toma de
[`OceanandWild/owsdatabase`](https://github.com/OceanandWild/owsdatabase) → `web/OWS`
y se copia a `app/` antes de compilar:

```powershell
npm run sync:app      # ../web/OWS -> ./app
```

En CI el mismo paso usa un checkout sparse de `owsdatabase/main`.

## Desarrollo

```powershell
npm install
npm run sync:app
npm run tauri dev     # usa devUrl http://localhost:8099
```

## Publicar una versión

```powershell
# 1) Bump de versión en src-tauri/tauri.conf.json + src-tauri/Cargo.toml + package.json
npm install
npm run tauri build            # genera NSIS + MSI en src-tauri/target/release/bundle

# 2) Publicar (requiere GITHUB_TOKEN con scope 'repo' y GH_REPO=OceanandWild/ows-hub)
$env:GH_REPO = "OceanandWild/ows-hub"
./scripts/publish-release.ps1 -Tag v3.1.4
```

O bien, desde GitHub Actions: `git push origin v3.1.4` dispara `.github/workflows/release.yml`,
que compila en `windows-latest` y publica la release con el instalador y `latest.json`.

### Updater

El updater de Tauri v2 lee el manifiesto estático publicado como asset de la
release más reciente:

```
https://github.com/OceanandWild/ows-hub/releases/latest/download/latest.json
```

La clave pública (`plugins.updater.pubkey` en `tauri.conf.json`) no se cambia: las
firmas las produce `tauri build` a partir de esa misma clave.

## Licencia

Privado — Ocean & Wild Studios.