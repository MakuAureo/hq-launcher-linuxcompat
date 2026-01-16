use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::mods;
use crate::mod_config::ModsConfig;
use crate::progress::{self, TaskErrorPayload, TaskFinishedPayload, TaskProgressPayload};
use crate::zip_utils;
use crate::downloader;
use progress::{emit_progress, emit_finished, emit_error};

// BepInEx installation via Thunderstore BepInExPack (Mono, preconfigured).
// We download the Thunderstore package zip and extract the contents of the `BepInExPack/` folder
// into the game root (versions/v{version}).
//
// Reference: https://thunderstore.io/c/lethal-company/p/BepInEx/BepInExPack/
const BEPINEXPACK_VERSION: &str = "5.4.2304";
const BEPINEXPACK_URL: &str =
    "https://thunderstore.io/package/download/BepInEx/BepInExPack/5.4.2304/";


fn overall_from_step(step: u32, step_progress: f64, steps_total: u32) -> f64 {
    let s = step.max(1).min(steps_total) as f64;
    let sp = step_progress.clamp(0.0, 1.0);
    (((s - 1.0) + sp) / (steps_total as f64)) * 100.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestState {
    manifest_version: u32,
}

fn manifest_state_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {e}"))?
        .join("config")
        .join("manifest_state.json"))
}

fn read_manifest_state(app: &tauri::AppHandle) -> Result<ManifestState, String> {
    let path = manifest_state_path(app)?;
    if !path.exists() {
        return Ok(ManifestState { manifest_version: 0 });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn write_manifest_state(app: &tauri::AppHandle, state: &ManifestState) -> Result<(), String> {
    let path = manifest_state_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

fn latest_installed_version_dir(
    app: &tauri::AppHandle,
) -> Result<Option<(u32, std::path::PathBuf)>, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {e}"))?
        .join("versions");

    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(None);
    };

    let mut best: Option<(u32, std::path::PathBuf)> = None;
    for e in rd {
        let Ok(e) = e else { continue };
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(num) = name.strip_prefix('v') else {
            continue;
        };
        let Ok(v) = num.parse::<u32>() else {
            continue;
        };
        if best.as_ref().map(|(bv, _)| v > *bv).unwrap_or(true) {
            best = Some((v, path));
        }
    }

    Ok(best)
}

fn shared_config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {e}"))?
        .join("config")
        .join("shared"))
}

fn copy_dir_add_only(src: &Path, dst: &Path) -> Result<(), String> {
    if src == dst {
        return Ok(());
    }
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(src), std::fs::canonicalize(dst)) {
        if a == b {
            return Ok(());
        }
    }

    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir_add_only(&from, &to)?;
            continue;
        }
        if ty.is_file() {
            if to.exists() {
                continue;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::MetadataExt;
    let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    Ok((md.file_attributes() & 0x400) != 0) // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_reparse_point(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

#[cfg(windows)]
fn create_dir_junction(link: &Path, target: &Path) -> Result<(), String> {
    let link_s = link.to_string_lossy().to_string();
    let target_s = target.to_string_lossy().to_string();

    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J", &link_s, &target_s])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("mklink /J failed: {stdout}{stderr}"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn create_dir_junction(link: &Path, target: &Path) -> Result<(), String> {
    // Best-effort fallback: create the directory (no junctions).
    let _ = target;
    std::fs::create_dir_all(link).map_err(|e| e.to_string())
}

/// Ensure `game_root/BepInEx/config` is a junction to the shared config directory.
///
/// Add-only behavior:
/// - If an old config dir exists, copy files into shared (skip existing), then replace with junction.
fn ensure_config_junction(app: &tauri::AppHandle, game_root: &Path) -> Result<PathBuf, String> {
    let shared = shared_config_dir(app)?;
    std::fs::create_dir_all(&shared).map_err(|e| e.to_string())?;

    let bepinex_dir = game_root.join("BepInEx");
    std::fs::create_dir_all(&bepinex_dir).map_err(|e| e.to_string())?;
    let link = bepinex_dir.join("config");

    // If it's already pointing to shared, do nothing.
    if link.exists() {
        if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&link), std::fs::canonicalize(&shared)) {
            if a == b {
                return Ok(shared);
            }
        }

        if link.is_dir() {
            // If it's a junction/symlink already, remove only the link itself.
            if is_reparse_point(&link)? {
                std::fs::remove_dir(&link).map_err(|e| e.to_string())?;
            } else {
                // Regular directory: copy into shared (add-only) then remove.
                let _ = copy_dir_add_only(&link, &shared);
                std::fs::remove_dir_all(&link).map_err(|e| e.to_string())?;
            }
        } else {
            // Unexpected file at the config path.
            std::fs::remove_file(&link).map_err(|e| e.to_string())?;
        }
    }

    create_dir_junction(&link, &shared)?;
    Ok(shared)
}

/// On app startup: compare local applied manifest version with remote manifest version.
/// If different, apply updates **additively** to the latest installed version (no overwrites).
pub async fn sync_latest_install_from_manifest(app: tauri::AppHandle) -> Result<(), String> {
    let Some((game_version, game_root)) = latest_installed_version_dir(&app)? else {
        return Ok(());
    };

    let client = reqwest::Client::new();
    let remote = ModsConfig::fetch_manifest(&client).await?;
    let (remote_manifest_version, mods_cfg, _chain_config, _manifests) = remote;

    let local_state = read_manifest_state(&app)?;
    if local_state.manifest_version == remote_manifest_version {
        log::info!("Manifest up-to-date: {}", remote_manifest_version);
        return Ok(());
    }

    log::info!(
        "Manifest changed: local={} remote={} -> applying additive updates",
        local_state.manifest_version,
        remote_manifest_version
    );

    // Two-step sync: config + mods (add-only).
    const STEPS_TOTAL: u32 = 2;
    let sync_res: Result<(), String> = async {

        // Step 1: config
        progress::emit_progress(
            &app,
            TaskProgressPayload {
                version: game_version,
                steps_total: STEPS_TOTAL,
                step: 1,
                step_name: "Sync Config".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(1, 0.0, STEPS_TOTAL),
                detail: Some("Downloading default_config.zip...".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: Some(0),
                total_files: None,
            },
        );

        let config_zip_url = "https://f.asta.rs/hq-launcher/default_config.zip";
        let cfg_bytes = client
            .get(config_zip_url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;

        let cfg_tmp_dir = game_root.join(".hq-launcher").join("tmp").join("config");
        std::fs::create_dir_all(&cfg_tmp_dir).map_err(|e| e.to_string())?;
        let cfg_zip_path = cfg_tmp_dir.join("default_config.zip");
        std::fs::write(&cfg_zip_path, &cfg_bytes).map_err(|e| e.to_string())?;

        // Ensure shared config junction, then extract into the shared dir (add-only).
        let shared_config = ensure_config_junction(&app, &game_root)?;
        let cfg_zip_path2 = cfg_zip_path.clone();
        let config_dir2 = shared_config.clone();
        let app_clone = app.clone();

        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            zip_utils::extract_config_zip_into_bepinex_config_with_progress(
                &cfg_zip_path2,
                &config_dir2,
                |done, total, name| {
                    let step_progress = if total == 0 {
                        1.0
                    } else {
                        (done as f64 / total as f64).clamp(0.0, 1.0)
                    };
                    let detail = name.map(|n| format!("{done}/{total} • {n}"));
                    progress::emit_progress(
                        &app_clone,
                        TaskProgressPayload {
                            version: game_version,
                            steps_total: STEPS_TOTAL,
                            step: 1,
                            step_name: "Sync Config".to_string(),
                            step_progress,
                            overall_percent: overall_from_step(1, step_progress, STEPS_TOTAL),
                            detail,
                            downloaded_bytes: None,
                            total_bytes: None,
                            extracted_files: Some(done),
                            total_files: Some(total),
                        },
                    );
                },
            )?;
            let _ = std::fs::remove_file(&cfg_zip_path2);
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;

        // Step 2: mods
        progress::emit_progress(
            &app,
            TaskProgressPayload {
                version: game_version,
                steps_total: STEPS_TOTAL,
                step: 2,
                step_name: "Sync Mods".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(2, 0.0, STEPS_TOTAL),
                detail: Some("Applying manifest...".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: Some(0),
                total_files: Some(mods_cfg.mods.len() as u64),
            },
        );

        mods::install_mods_with_progress(&game_root, game_version, &mods_cfg, |done, total, detail| {
            let step_progress = if total == 0 {
                1.0
            } else {
                (done as f64 / total as f64).clamp(0.0, 1.0)
            };

            progress::emit_progress(
                &app,
                TaskProgressPayload {
                    version: game_version,
                    steps_total: STEPS_TOTAL,
                    step: 2,
                    step_name: "Sync Mods".to_string(),
                    step_progress,
                    overall_percent: overall_from_step(2, step_progress, STEPS_TOTAL),
                    detail,
                    downloaded_bytes: None,
                    total_bytes: None,
                    extracted_files: Some(done),
                    total_files: Some(total),
                },
            );
        })
        .await?;

        // Mark sync as complete for the UI.
        progress::emit_progress(
            &app,
            TaskProgressPayload {
                version: game_version,
                steps_total: STEPS_TOTAL,
                step: 2,
                step_name: "Sync Mods".to_string(),
                step_progress: 1.0,
                overall_percent: 100.0,
                detail: Some("Sync complete".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        write_manifest_state(
            &app,
            &ManifestState {
                manifest_version: remote_manifest_version,
            },
        )?;

        Ok(())
    }
    .await;

    match sync_res {
        Ok(()) => {
            progress::emit_finished(
                &app,
                progress::TaskFinishedPayload {
                    version: game_version,
                    path: game_root.to_string_lossy().to_string(),
                },
            );
            Ok(())
        }
        Err(e) => {
            progress::emit_error(
                &app,
                progress::TaskErrorPayload {
                    version: game_version,
                    message: e.clone(),
                },
            );
            Err(e)
        }
    }
}

pub async fn download_and_setup(app: tauri::AppHandle, version: u32) -> Result<bool, String> {
    let res: Result<bool, String> = async {
        // DepotDownloader 설치 확인
        if let Err(e) = downloader::install_downloader(&app).await {
            return Err(format!("Failed to install DepotDownloader: {e}"));
        }

        let client = reqwest::Client::new();

        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve app data dir: {e}"))?
            .join("versions");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let extract_dir = dir.join(format!("v{version}"));

        // Download -> Extract Game -> Install BepInEx -> Install Config -> Install Mods
        const STEPS_TOTAL: u32 = 5;

        // Step 1: Steam 로그인 확인
        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 1,
                step_name: "Login Check".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(1, 0.0, STEPS_TOTAL),
                detail: Some("Checking Steam login...".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        let downloader = downloader::DepotDownloader::new(&app)?;
        let login_state = downloader.get_login_state();
        
        if !login_state.is_logged_in {
            return Err("Not logged in to Steam. Please login first.".to_string());
        }

        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 1,
                step_name: "Login Check".to_string(),
                step_progress: 1.0,
                overall_percent: overall_from_step(1, 1.0, STEPS_TOTAL),
                detail: Some(format!("Logged in as {}", login_state.username.unwrap_or_default())),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        // Fetch remote manifest data (mods + per-game-version depots manifest ids).
        let (_remote_manifest_version, mods_cfg, _chain_config, manifests) =
            ModsConfig::fetch_manifest(&client).await?;

        // Step 2: Lethal Company 다운로드
        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 2,
                step_name: "Download Game".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(2, 0.0, STEPS_TOTAL),
                detail: Some("Starting download...".to_string()),
                downloaded_bytes: Some(0),
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        if extract_dir.exists() {
            std::fs::remove_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;

        log::info!("Downloading Lethal Company to {}", extract_dir.display());

        let manifest_id = manifests
            .get(&version)
            .cloned()
            .ok_or_else(|| format!("No depot manifest id for game version {version} in remote manifest."))?;

        // 게임 다운로드
        downloader.download_depot(
            Some(manifest_id),
            extract_dir.clone(),
        ).await?;

        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 2,
                step_name: "Download Game".to_string(),
                step_progress: 1.0,
                overall_percent: overall_from_step(2, 1.0, STEPS_TOTAL),
                detail: Some("Download complete".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        // Step 3: BepInEx 다운로드 및 설치
        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 3,
                step_name: "Install BepInEx".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(3, 0.0, STEPS_TOTAL),
                detail: Some("Downloading BepInEx...".to_string()),
                downloaded_bytes: Some(0),
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        log::info!(
            "Downloading BepInExPack {} from {}",
            BEPINEXPACK_VERSION,
            BEPINEXPACK_URL
        );

        let response = client
            .get(BEPINEXPACK_URL)
            .header("User-Agent", "hq-launcher/0.1 (tauri)")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;

        let total = response.content_length();
        let temp_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("Failed to resolve app data dir: {e}"))?
            .join("temp");
        std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

        let zip_path = temp_dir.join(format!("bepinexpack_{BEPINEXPACK_VERSION}.zip"));
        let mut file = File::create(&zip_path).map_err(|e| e.to_string())?;

        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);

            let step_progress = total
                .map(|t| if t == 0 { 0.0 } else { (downloaded as f64 / t as f64).clamp(0.0, 1.0) })
                .unwrap_or(0.0);

            emit_progress(
                &app,
                TaskProgressPayload {
                    version,
                    steps_total: STEPS_TOTAL,
                    step: 3,
                    step_name: "Install BepInEx".to_string(),
                    step_progress: step_progress * 0.5, // download = 0~50%
                    overall_percent: overall_from_step(3, step_progress * 0.5, STEPS_TOTAL),
                    detail: Some(format!(
                        "Downloading BepInExPack... {} MB",
                        downloaded / 1024 / 1024
                    )),
                    downloaded_bytes: Some(downloaded),
                    total_bytes: total,
                    extracted_files: None,
                    total_files: None,
                },
            );
        }
        drop(file);

        // Basic sanity check: ZIP files start with "PK". If not, we likely downloaded an HTML error page.
        {
            use std::io::Read as _;
            let mut f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
            let mut header = [0u8; 4];
            let n = f.read(&mut header).map_err(|e| e.to_string())?;
            if n < 2 || header[0] != b'P' || header[1] != b'K' {
                let _ = std::fs::remove_file(&zip_path);
                return Err("BepInExPack download is not a valid zip (got non-zip response). Please retry.".to_string());
            }
        }

        // Extract Thunderstore package into the game root.
        // Thunderstore zips contain top-level files (manifest.json, icon.png) and a top-level folder (BepInExPack/).
        // This extractor strips the top-level dir and ignores the top-level files, resulting in:
        // - winhttp.dll, doorstop_config.ini, BepInEx/**, etc directly under versions/v{version}.
        let zip_path_clone = zip_path.clone();
        let extract_dir_clone = extract_dir.clone();
        let app_clone = app.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            zip_utils::extract_thunderstore_package_with_progress(
                &zip_path_clone,
                &extract_dir_clone,
                |done, total, detail| {
                    let step_progress = if total == 0 {
                        1.0
                    } else {
                        (done as f64 / total as f64).clamp(0.0, 1.0)
                    };
                    let step_progress = 0.5 + (step_progress * 0.5); // extract = 50~100%
                    emit_progress(
                        &app_clone,
                        TaskProgressPayload {
                            version,
                            steps_total: STEPS_TOTAL,
                            step: 3,
                            step_name: "Install BepInEx".to_string(),
                            step_progress,
                            overall_percent: overall_from_step(3, step_progress, STEPS_TOTAL),
                            detail: detail.map(|d| format!("Extracting BepInExPack... {d}")),
                            downloaded_bytes: None,
                            total_bytes: None,
                            extracted_files: Some(done),
                            total_files: Some(total),
                        },
                    );
                },
            )?;
            let _ = std::fs::remove_file(&zip_path_clone);
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;

        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 3,
                step_name: "Install BepInEx".to_string(),
                step_progress: 1.0,
                overall_percent: overall_from_step(3, 1.0, STEPS_TOTAL),
                detail: Some(format!("BepInExPack {} installed", BEPINEXPACK_VERSION)),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        // Step 4: Config 설치
        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 4,
                step_name: "Install Config".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(4, 0.0, STEPS_TOTAL),
                detail: Some("Setting up config...".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        // Config directory is a junction to AppData/config/shared.
        // This also seeds shared config additively from any extracted config files.
        let _shared = ensure_config_junction(&app, &extract_dir)?;

        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 4,
                step_name: "Install Config".to_string(),
                step_progress: 1.0,
                overall_percent: overall_from_step(4, 1.0, STEPS_TOTAL),
                detail: Some("Config ready".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        // Step 5: Mods 설치
        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 5,
                step_name: "Install Mods".to_string(),
                step_progress: 0.0,
                overall_percent: overall_from_step(5, 0.0, STEPS_TOTAL),
                detail: Some("Installing plugins...".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: Some(0),
                total_files: None,
            },
        );

        let plugins_dir = mods::plugins_dir(&extract_dir);
        std::fs::create_dir_all(&plugins_dir).map_err(|e| e.to_string())?;

        mods::install_mods_with_progress(&extract_dir, version, &mods_cfg, |done, total, detail| {
            let step_progress = if total == 0 {
                1.0
            } else {
                (done as f64 / total as f64).clamp(0.0, 1.0)
            };
            emit_progress(
                &app,
                TaskProgressPayload {
                    version,
                    steps_total: STEPS_TOTAL,
                    step: 5,
                    step_name: "Install Mods".to_string(),
                    step_progress,
                    overall_percent: overall_from_step(5, step_progress, STEPS_TOTAL),
                    detail,
                    downloaded_bytes: None,
                    total_bytes: None,
                    extracted_files: Some(done),
                    total_files: Some(total),
                },
            );
        })
        .await?;

        emit_progress(
            &app,
            TaskProgressPayload {
                version,
                steps_total: STEPS_TOTAL,
                step: 5,
                step_name: "Install Mods".to_string(),
                step_progress: 1.0,
                overall_percent: overall_from_step(5, 1.0, STEPS_TOTAL),
                detail: Some("Mods installed".to_string()),
                downloaded_bytes: None,
                total_bytes: None,
                extracted_files: None,
                total_files: None,
            },
        );

        emit_finished(
            &app,
            TaskFinishedPayload {
                version,
                path: extract_dir.to_string_lossy().to_string(),
            },
        );

        log::info!("Setup completed for version {}", version);
        Ok(true)
    }
    .await;

    if let Err(message) = &res {
        emit_error(
            &app,
            TaskErrorPayload {
                version,
                message: message.clone(),
            },
        );
    }

    res
}