//! OWS Hub — backend Tauri (biblioteca local estilo launcher).
//! Comandos: carpeta de biblioteca, descarga con progreso,
//! extracción de ZIP, búsqueda del .exe real y lanzamiento.
//!
//! Flujo Wilder Gambit (automático, 1 clic):
//!   download_installer (ZIP real de itch.io vía servidor)
//!   → extract_zip (todo el contenido)
//!   → find_game_exe (detecta el .exe jugable, ignora crash-handler/stockfish)
//!   → launch_game (ejecuta sin que el usuario haga nada más)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

/// %APPDATA%/OWS/library — raíz de juegos instalados.
fn library_root(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(base.join("OWS").join("library"))
}

#[tauri::command]
async fn get_library_dir(app: AppHandle) -> Result<String, String> {
    let dir = library_root(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

fn sanitize_slug(raw: &str) -> String {
    let clean: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        "game".to_string()
    } else {
        clean
    }
}

fn sanitize_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("installer.zip")
        .trim();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        "installer.zip".to_string()
    } else {
        clean
    }
}

/// Extrae filename="..." de Content-Disposition (soporta filename*=UTF-8''...).
fn filename_from_content_disposition(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    let v = value?.to_str().ok()?;
    // filename*=UTF-8''Wilder-Gambit-v0.1.0-Windows.zip
    if let Some(idx) = v.find("filename*=") {
        let rest = v[idx + "filename*=".len()..].trim().trim_matches('"').trim();
        // formato: UTF-8''nombre — nos quedamos con lo que hay tras ''
        let name = rest.split("''").last().unwrap_or(rest).trim();
        let name = name.split(';').next().unwrap_or(name).trim().trim_matches('"');
        if !name.is_empty() {
            return Some(sanitize_filename(name));
        }
    }
    if let Some(idx) = v.find("filename=") {
        let rest = v[idx + "filename=".len()..].trim();
        let rest = rest.trim_start_matches('"');
        let end = rest.find(['"', ';']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        if !name.is_empty() {
            return Some(sanitize_filename(name));
        }
    }
    None
}

fn looks_like_installer_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".zip")
        || lower.ends_with(".exe")
        || lower.ends_with(".msi")
        || lower.ends_with(".apk")
        || lower.ends_with(".aab")
        || lower.ends_with(".x86_64")
}

/// Descarga el ZIP real del juego a library/<slug>/ emitiendo
/// eventos `ows-download-progress` { slug, downloaded, total }.
///
/// El endpoint del servidor (`/ows-launch-projects/:slug/download`)
/// ya resuelve itch.io vía ITCH_API_KEY y devuelve el ZIP binario
/// con `Content-Disposition: attachment; filename="*.zip"`.
/// Aquí respetamos ese filename; si la URL es genérica (`.../download`
/// sin extensión) caemos a `<slug>.zip` para que `extract_zip` funcione.
#[tauri::command]
async fn download_installer(app: AppHandle, slug: String, url: String) -> Result<String, String> {
    let slug = sanitize_slug(&slug);
    let dir = library_root(&app)?.join(&slug);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let res = reqwest::get(&url)
        .await
        .map_err(|e| format!("download request: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }

    let total = res.content_length().unwrap_or(0);
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let disp_name = filename_from_content_disposition(
        res.headers().get(reqwest::header::CONTENT_DISPOSITION),
    );

    let url_name_raw = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    let url_name = if looks_like_installer_file(url_name_raw) {
        Some(sanitize_filename(url_name_raw))
    } else {
        None
    };

    let mut filename = disp_name
        .clone()
        .filter(|n| looks_like_installer_file(n))
        .or(url_name)
        .or(disp_name)
        .unwrap_or_else(|| format!("{slug}.zip"));

    // Si el servidor dice que es un zip pero el nombre no tiene extensión,
    // forzamos .zip (caso `.../download` sin Content-Disposition).
    if (content_type.contains("zip") || content_type.contains("octet-stream"))
        && !filename.contains('.')
    {
        filename.push_str(".zip");
    }
    if filename.eq_ignore_ascii_case("download") || filename.eq_ignore_ascii_case("installer.bin") {
        filename = format!("{slug}.zip");
    }

    let dest = dir.join(&filename);

    let mut stream = res.bytes_stream();
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| e.to_string())?;

    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download chunk: {e}"))?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        let _ = app.emit(
            "ows-download-progress",
            serde_json::json!({ "slug": slug, "downloaded": downloaded, "total": total }),
        );
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    // ── Validación: debe ser un ZIP real, no una página HTML de error ──
    let meta = std::fs::metadata(&dest).map_err(|e| e.to_string())?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(&dest);
        return Err("descarga vacía (0 bytes)".to_string());
    }
    let mut head = [0u8; 4];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(&dest).map_err(|e| e.to_string())?;
        let n = f.read(&mut head).map_err(|e| e.to_string())?;
        if n < 4 || &head[0..2] != b"PK" {
            let _ = std::fs::remove_file(&dest);
            if content_type.contains("text/html") || (n >= 1 && head[0] == b'<') {
                return Err(
                    "el servidor devolvió HTML en vez del ZIP (¿juego sin instalador o itch restringido?)"
                        .to_string(),
                );
            }
            return Err("el archivo descargado no es un ZIP válido".to_string());
        }
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Extrae un ZIP a la carpeta destino (protegido contra zip-slip).
#[tauri::command]
fn extract_zip(zip_path: String, dest_dir: String) -> Result<String, String> {
    let file = std::fs::File::open(&zip_path)
        .map_err(|e| format!("no se pudo abrir el ZIP: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("ZIP inválido (¿descarga incompleta?): {e}"))?;
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let out = match entry.enclosed_name() {
            Some(p) => PathBuf::from(&dest_dir).join(p),
            None => continue,
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out_file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out_file).map_err(|e| e.to_string())?;
        }
    }
    Ok(dest_dir)
}

fn exe_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn is_ignored_exe(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase();
    // Crash-handler de Unity, desinstaladores y binarios internos no son el juego.
    if lower.contains("unitycrashhandler")
        || lower.contains("crashhandler")
        || lower.contains("unins")
        || lower.contains("uninstall")
    {
        return true;
    }
    // Motor de ajedrez interno de Wilder Gambit y plugins: nunca lanzar.
    if lower.contains("streamingassets") || lower.contains("stockfish") || lower.contains("monobleedingedge") {
        return true;
    }
    // Carpetas internas de Unity
    if lower.contains("_data/") || lower.contains("_data\\") {
        return true;
    }
    false
}

fn has_data_sibling(exe: &Path) -> bool {
    let stem = exe_stem(exe);
    if stem.is_empty() {
        return false;
    }
    match exe.parent() {
        Some(parent) => parent.join(format!("{stem}_Data")).is_dir(),
        None => false,
    }
}

fn exe_file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Busca el .exe jugable dentro de la carpeta extraída.
///
/// Prioridad:
///  1. `preferred` exacto (ej. "Wilder Gambit.exe") si existe.
///  2. .exe en la raíz (sin crash-handler) con carpeta `<nombre>_Data` al lado (caso Unity).
///  3. .exe en raíz más grande (descarta uninstallers pequeños).
///  4. Búsqueda recursiva (máx. 4 niveles, salta StreamingAssets) con la misma prioridad.
#[tauri::command]
fn find_game_exe(dest_dir: String, preferred: Option<String>) -> Result<String, String> {
    let root = PathBuf::from(&dest_dir);
    if !root.is_dir() {
        return Err(format!("no existe la carpeta instalada: {dest_dir}"));
    }

    // 1) Preferido exacto
    if let Some(pref) = preferred {
        let pref = pref.trim();
        if !pref.is_empty() {
            let candidate = root.join(pref);
            if candidate.is_file() && !is_ignored_exe(&candidate) {
                return Ok(candidate.to_string_lossy().to_string());
            }
            // Intento insensible a mayúsculas en la raíz
            if let Ok(entries) = std::fs::read_dir(&root) {
                let want = pref.to_lowercase();
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_file()
                        && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase() == want)
                            .unwrap_or(false)
                    {
                        return Ok(p.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // 2) y 3) .exe en la raíz
    let mut root_exes: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file()
                && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                && !is_ignored_exe(&p)
            {
                root_exes.push(p);
            }
        }
    }
    if root_exes.len() == 1 {
        return Ok(root_exes[0].to_string_lossy().to_string());
    }
    if !root_exes.is_empty() {
        // Prefiere el que tenga _Data al lado (build Unity real)
        let mut with_data: Vec<&PathBuf> = root_exes.iter().filter(|p| has_data_sibling(p)).collect();
        if with_data.len() == 1 {
            return Ok(with_data[0].to_string_lossy().to_string());
        }
        if !with_data.is_empty() {
            with_data.sort_by_key(|p| std::cmp::Reverse(exe_file_size(p)));
            return Ok(with_data[0].to_string_lossy().to_string());
        }
        // Sin _Data: el más grande (evita uninstallers / helpers pequeños)
        root_exes.sort_by_key(|p| std::cmp::Reverse(exe_file_size(p)));
        return Ok(root_exes[0].to_string_lossy().to_string());
    }

    // 4) Recursivo (carpetas con nombre de versión, builds anidados, etc.)
    let mut stack: Vec<(PathBuf, u8)> = vec![(root.clone(), 0)];
    let mut found: Vec<(PathBuf, u8)> = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        if depth > 4 {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
                if name == "streamingassets"
                    || name == "__macosx"
                    || name.starts_with('.')
                    || name == "monobleedingedge"
                {
                    continue;
                }
                stack.push((p, depth + 1));
            } else if p.is_file()
                && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false)
                && !is_ignored_exe(&p)
            {
                found.push((p, depth));
            }
        }
    }
    if found.is_empty() {
        return Err("no se encontró ningún .exe del juego en la carpeta extraída".to_string());
    }
    // Prefiere con _Data, luego menor profundidad, luego mayor tamaño
    found.sort_by(|a, b| {
        let da = has_data_sibling(&a.0);
        let db = has_data_sibling(&b.0);
        db.cmp(&da)
            .then(a.1.cmp(&b.1))
            .then(exe_file_size(&b.0).cmp(&exe_file_size(&a.0)))
    });
    Ok(found[0].0.to_string_lossy().to_string())
}

/// Lanza el .exe del juego (cwd = carpeta del exe, como un launcher).
#[tauri::command]
fn launch_game(exe_path: String) -> Result<(), String> {
    let exe = Path::new(&exe_path);
    if !exe.is_file() {
        return Err(format!("no existe el ejecutable: {exe_path}"));
    }
    let cwd = exe.parent().unwrap_or(Path::new("."));
    std::process::Command::new(exe)
        .current_dir(cwd)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ═══════════════════════════════════════════════════════
// DESINSTALAR
//
// Borrar desde el frontend es la operación más peligrosa del Hub: `dir` viene
// del WebView y podría ser cualquier ruta del disco. Por eso solo se acepta
// una carpeta que sea HIJA DIRECTA de una raíz de biblioteca conocida (la
// por defecto %APPDATA%/OWS/library o la personalizada en el setup), nunca la
// raíz ni una carpeta fuera, y nunca a través de un enlace simbólico.
// ═══════════════════════════════════════════════════════

/// Tamaño total en disco de una carpeta (bytes). No sigue enlaces simbólicos.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_symlink() => continue, // nunca atravesar un link
                Ok(ft) if ft.is_dir() => stack.push(p),
                Ok(_) => {
                    if let Ok(m) = e.metadata() {
                        total += m.len();
                    }
                }
                Err(_) => continue,
            }
        }
    }
    total
}

/// Resuelve la ruta real (absoluta, con symlinks resueltos) para comparar.
fn real_path(p: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(p).map_err(|e| format!("no se pudo resolver {}: {e}", p.display()))
}

/// Devuelve la carpeta a borrar solo si es una carpeta de juego legítima.
fn safe_game_dir(
    app: &AppHandle,
    dir: &str,
    library_base: Option<&str>,
) -> Result<PathBuf, String> {
    let raw = dir.trim();
    if raw.is_empty() {
        return Err("no se indicó qué juego desinstalar".to_string());
    }
    let target = real_path(Path::new(raw))?;

    let mut roots: Vec<PathBuf> = Vec::new();
    let default_root = library_root(app)?;
    if default_root.exists() {
        roots.push(real_path(&default_root)?);
    }
    // La biblioteca personalizada del setup es una raíz válida también.
    if let Some(base) = library_base {
        let b = PathBuf::from(base.trim());
        if b.exists() {
            roots.push(real_path(&b)?);
        }
    }
    if roots.is_empty() {
        return Err("no se pudo resolver la biblioteca de juegos del Hub".to_string());
    }

    for root in roots {
        // Solo hijas directas de la raíz (una carpeta = un juego).
        if target.parent() != Some(root.as_path()) {
            continue;
        }
        if !target.starts_with(&root) {
            continue;
        }
        if !target.is_dir() {
            return Err("esa carpeta de juego ya no existe".to_string());
        }
        return Ok(target);
    }

    Err("solo se puede desinstalar una carpeta de juego de la biblioteca del Hub".to_string())
}

/// Tamaño en disco de la carpeta de un juego (bytes). 0 si no existe.
#[tauri::command]
fn game_dir_size(dir: String) -> Result<u64, String> {
    let p = PathBuf::from(dir.trim());
    if !p.is_dir() {
        return Ok(0);
    }
    Ok(dir_size(&p))
}

/// Borra la carpeta de un juego instalado y devuelve lo que quedó libre.
/// Reintenta unos segundos porque Windows bloquea el borrado si el juego (o su
/// lanzador) sigue abierto.
#[tauri::command]
async fn uninstall_game(
    app: AppHandle,
    dir: String,
    library_base: Option<String>,
) -> Result<serde_json::Value, String> {
    let target = safe_game_dir(&app, &dir, library_base.as_deref())?;
    let bytes = dir_size(&target);
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let _ = app.emit(
        "ows-uninstall-progress",
        serde_json::json!({ "phase": "removing", "dir": target.to_string_lossy(), "bytes": bytes }),
    );

    let to_remove = target.clone();
    let removed: Result<(), String> = tauri::async_runtime::spawn_blocking(move || {
        let mut last = String::new();
        for attempt in 0..5u64 {
            match std::fs::remove_dir_all(&to_remove) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = e.to_string();
                    std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1)));
                }
            }
        }
        Err(last)
    })
    .await
    .map_err(|e| e.to_string())?;

    if let Err(err) = removed {
        let lower = err.to_lowercase();
        if lower.contains("denied")
            || lower.contains("being used")
            || lower.contains("used by another process")
            || lower.contains("cannot find")
        {
            return Err(
                "No se pudo borrar: el juego está abierto. Ciérralo y vuelve a intentarlo."
                    .to_string(),
            );
        }
        return Err(format!("No se pudo borrar la carpeta del juego: {err}"));
    }

    let _ = app.emit(
        "ows-uninstall-progress",
        serde_json::json!({ "phase": "done", "dir": name }),
    );
    Ok(serde_json::json!({ "dir": name, "bytes": bytes }))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_library_dir,
            download_installer,
            extract_zip,
            find_game_exe,
            launch_game,
            game_dir_size,
            uninstall_game
        ])
        .run(tauri::generate_context!())
        .expect("error while running OWS Hub");
}
