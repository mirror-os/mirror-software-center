// SPDX-License-Identifier: GPL-3.0-only
//
// Mirror OS catalog backend.
//
// Loads app metadata from the SQLite catalog DB at
// ~/.local/share/mirror-os/catalog.db (maintained by mirror-catalog-update).
//
// Responsibilities:
//   - Expose all Flatpak + Nix app metadata via catalog_infos()
//   - Resolve icons from the local media cache
//     (~/.local/share/mirror-os/media/icons/)
//   - List installed apps via `mirror-os list --json`
//   - Install / uninstall via `mirror-os install` / `mirror-os uninstall`
//   - Provide FTS5 search via fts_search()

use cosmic::widget;
use rusqlite::{Connection, OpenFlags, params};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use crate::{
    appstream_cache::AppstreamCache,
    operation::{Operation, OperationKind},
    app_id::AppId,
    app_info::{AppIcon, AppInfo, AppKind, AppRelease, AppScreenshot, AppUrl},
};

use super::{Backend, Package};

// ── CatalogBackend ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CatalogBackend {
    /// Path to the SQLite catalog DB
    db_path: PathBuf,
    /// Base directory for locally-cached media (~/.local/share/mirror-os/media)
    media_dir: PathBuf,
    /// In-memory app info map populated by load_caches()
    pub infos: HashMap<AppId, Arc<AppInfo>>,
}

impl CatalogBackend {
    pub fn new(db_path: PathBuf, media_dir: PathBuf) -> Self {
        Self {
            db_path,
            media_dir,
            infos: HashMap::new(),
        }
    }
}

// ── FTS5 search ───────────────────────────────────────────────────────────────

/// Run an FTS5 search against the catalog DB.
///
/// Returns `(source, id)` pairs ordered by relevance (BM25 rank ascending),
/// with monthly_downloads as secondary sort for Flatpak results.
pub fn fts_search(
    db_path: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    // Build FTS5 match expression: each token becomes "token"* (prefix match)
    let fts_query: String = query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");

    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT f.source, f.id
         FROM catalog_fts f
         LEFT JOIN flatpak_apps fa ON f.source = 'flatpak' AND f.id = fa.app_id
         WHERE catalog_fts MATCH ?1
         ORDER BY f.rank ASC, COALESCE(fa.monthly_downloads, 0) DESC
         LIMIT ?2",
    )?;

    let results = stmt
        .query_map(params![fts_query, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

// ── AppInfo construction helpers ──────────────────────────────────────────────

fn parse_json_strings(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

fn build_flatpak_info(
    app_id: &str,
    name: &str,
    summary: &str,
    description: &str,
    version: &str,
    developer: &str,
    license: &str,
    homepage: &str,
    bugtracker_url: &str,
    donation_url: &str,
    categories_json: &str,
    icon_name: &str,
    icon_local_path: &str,
    screenshots_json: &str,
    content_rating: &str,
    flatpak_ref: &str,
    releases_json: &str,
    verified: bool,
    monthly_downloads: u64,
) -> AppInfo {
    let categories = parse_json_strings(categories_json);

    let icons = {
        let mut v: Vec<AppIcon> = Vec::new();
        if !icon_local_path.is_empty() {
            v.push(AppIcon::Local(icon_local_path.to_string(), Some(128), Some(128), None));
        } else if !icon_name.is_empty() {
            // Fall back to stock icon name (many apps register their app-id as an XDG icon)
            v.push(AppIcon::Stock(icon_name.to_string()));
        }
        if v.is_empty() {
            v.push(AppIcon::Stock("package-x-generic".to_string()));
        }
        v
    };

    let screenshots: Vec<AppScreenshot> = serde_json::from_str::<Vec<serde_json::Value>>(screenshots_json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| {
            // Prefer locally cached path; fall back to remote URL
            let local = s["local_path"].as_str().unwrap_or("").to_string();
            let remote = s["url"].as_str().unwrap_or("").to_string();
            let path = if !local.is_empty() { local } else { remote };
            if path.is_empty() {
                return None;
            }
            let caption = s["caption"].as_str().unwrap_or("").to_string();
            Some(AppScreenshot { caption, url: path })
        })
        .collect();

    let releases: Vec<AppRelease> = serde_json::from_str::<Vec<serde_json::Value>>(releases_json)
        .unwrap_or_default()
        .into_iter()
        .map(|r| AppRelease {
            timestamp: r["timestamp"].as_i64().filter(|&t| t > 0),
            version: r["version"].as_str().unwrap_or("").to_string(),
            description: r["description"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
            url: None,
        })
        .collect();

    let mut urls: Vec<AppUrl> = Vec::new();
    if !homepage.is_empty() {
        urls.push(AppUrl::Homepage(homepage.to_string()));
    }
    if !bugtracker_url.is_empty() {
        urls.push(AppUrl::BugTracker(bugtracker_url.to_string()));
    }
    if !donation_url.is_empty() {
        urls.push(AppUrl::Donation(donation_url.to_string()));
    }

    let flatpak_refs = if !flatpak_ref.is_empty() {
        vec![flatpak_ref.to_string()]
    } else {
        vec![]
    };

    AppInfo {
        source_id: "flathub".to_string(),
        source_name: "Flathub".to_string(),
        origin_opt: Some("flathub".to_string()),
        name: name.to_string(),
        summary: summary.to_string(),
        kind: AppKind::DesktopApplication,
        developer_name: developer.to_string(),
        description: description.to_string(),
        license_opt: if !license.is_empty() { Some(license.to_string()) } else { None },
        pkgnames: vec![],
        package_paths: vec![],
        categories,
        desktop_ids: vec![app_id.to_string()],
        flatpak_refs,
        icons,
        provides: vec![],
        releases,
        screenshots,
        urls,
        monthly_downloads,
        is_verified: verified,
        content_rating: content_rating.to_string(),
    }
}

/// Scan system and user icon theme directories once at startup and return the
/// set of base icon names (without extension) that actually exist on disk.
///
/// This is used to avoid calling `widget::icon::from_name()` for icon names
/// that are absent from the theme — COSMIC traverses every theme directory
/// before giving up on a missing name, which is very slow when done for
/// hundreds of Nix packages.
fn scan_available_icon_names() -> HashSet<String> {
    let mut names = HashSet::new();

    // Build the list of directories to scan.
    let mut hicolor_roots: Vec<PathBuf> = vec![
        PathBuf::from("/usr/share/icons/hicolor"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        hicolor_roots.push(home.join(".nix-profile/share/icons/hicolor"));
        hicolor_roots.push(home.join(".local/share/icons/hicolor"));
    }

    // For each hicolor root, walk every size subdirectory's apps/ folder.
    for root in &hicolor_roots {
        if let Ok(size_dirs) = std::fs::read_dir(root) {
            for size_dir in size_dirs.flatten() {
                collect_icon_stems_from_dir(&size_dir.path().join("apps"), &mut names);
            }
        }
    }

    // /usr/share/pixmaps — legacy flat directory
    collect_icon_stems_from_dir(Path::new("/usr/share/pixmaps"), &mut names);

    names
}

fn collect_icon_stems_from_dir(dir: &Path, names: &mut HashSet<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let s = fname.to_string_lossy();
            // Strip the last extension to get the icon stem (e.g. "gimp.png" → "gimp")
            let stem = match s.rfind('.') {
                Some(pos) => &s[..pos],
                None => &s,
            };
            names.insert(stem.to_string());
        }
    }
}

/// Info extracted from a single `.desktop` file relevant to the catalog backend.
struct DesktopFileInfo {
    /// Stem of the desktop file, e.g. `"winboard"` from `"winboard.desktop"`.
    desktop_stem: String,
    /// Value of the `Icon=` field, if present, e.g. `"winboard"`.
    icon_name: Option<String>,
}

/// Scan XDG `share/applications/` directories (Nix profile + system) and return
/// a map keyed by the *lowercase* desktop stem for fast pname/attr lookup.
///
/// This is used to:
///   1. Populate `AppInfo::desktop_ids` so the "Open" button works for Nix apps.
///   2. Pick the icon name from `Icon=` rather than guessing from pname, so apps
///      like `winboard` (whose icon is also `"winboard"` but pname might differ)
///      are handled correctly.
fn scan_nix_desktop_files() -> HashMap<String, DesktopFileInfo> {
    let mut map = HashMap::new();

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        // Nix home-manager profile (most important for mirror-os install)
        search_dirs.push(home.join(".nix-profile/share/applications"));
        search_dirs.push(home.join(".local/share/applications"));
    }
    // System applications (fallback — Flatpaks go here via flatpak run wrappers)
    search_dirs.push(PathBuf::from("/usr/share/applications"));

    for dir in &search_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(stem_os) = path.file_stem() else { continue };
            let stem = stem_os.to_string_lossy().to_string();
            if stem.is_empty() {
                continue;
            }
            let icon_name = read_desktop_icon_field(&path);
            let key = stem.to_lowercase();
            // First match wins (Nix profile takes priority over system).
            map.entry(key).or_insert(DesktopFileInfo {
                desktop_stem: stem,
                icon_name,
            });
        }
    }
    map
}

/// Read only the `Icon=` line from a `.desktop` file without parsing the whole file.
fn read_desktop_icon_field(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        if in_desktop_entry {
            if trimmed.starts_with('[') {
                break; // entered a different section
            }
            if let Some(val) = trimmed.strip_prefix("Icon=") {
                let icon = val.trim().to_string();
                if !icon.is_empty() {
                    return Some(icon);
                }
            }
        }
    }
    None
}

fn build_nix_info(
    attr: &str,
    pname: &str,
    version: &str,
    description: &str,
    long_description: &str,
    homepage: &str,
    license: &str,
    icon_name: &str,
    desktop_ids: Vec<String>,
) -> AppInfo {
    let urls = if !homepage.is_empty() {
        vec![AppUrl::Homepage(homepage.to_string())]
    } else {
        vec![]
    };

    let summary = if description.chars().count() <= 120 {
        description.to_string()
    } else {
        let truncated: String = description.chars().take(117).collect();
        format!("{}…", truncated)
    };
    let full_desc = if !long_description.is_empty() {
        long_description.to_string()
    } else {
        description.to_string()
    };

    AppInfo {
        source_id: "nixpkgs".to_string(),
        source_name: "nixpkgs".to_string(),
        origin_opt: None,
        name: if !pname.is_empty() { pname.to_string() } else { attr.to_string() },
        summary,
        kind: AppKind::DesktopApplication,
        developer_name: String::new(),
        description: full_desc,
        license_opt: if !license.is_empty() { Some(license.to_string()) } else { None },
        pkgnames: vec![attr.to_string()],
        package_paths: vec![],
        categories: vec![],
        desktop_ids,
        flatpak_refs: vec![],
        icons: vec![AppIcon::Stock(icon_name.to_string())],
        provides: vec![],
        releases: vec![],
        screenshots: vec![],
        urls,
        monthly_downloads: 0,
        is_verified: false,
        content_rating: "all".to_string(),
    }
}

// ── Backend trait implementation ──────────────────────────────────────────────

impl Backend for CatalogBackend {
    fn load_caches(&mut self, _refresh: bool) -> Result<(), Box<dyn Error>> {
        log::info!("catalog: opening DB at {}", self.db_path.display());
        let conn = Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        log::info!("catalog: DB opened successfully");

        self.infos.clear();

        // ── Nix packages ──────────────────────────────────────────────────────
        {
            log::info!("catalog: loading nix_packages...");
            let t = std::time::Instant::now();

            // Scan icon theme dirs so we never call from_name() for a name that
            // doesn't exist — COSMIC's missing-icon traversal is slow (~50 ms each).
            let available_icons = scan_available_icon_names();
            log::info!(
                "catalog: found {} icon names in system theme dirs",
                available_icons.len()
            );

            // Scan desktop files in the Nix profile to get accurate icon names
            // (from Icon= field) and populate desktop_ids for the Open button.
            let desktop_files = scan_nix_desktop_files();
            log::info!(
                "catalog: found {} desktop files in Nix profile / system",
                desktop_files.len()
            );

            let mut stmt = conn.prepare(
                "SELECT attr, pname, version, description, long_description, homepage, license
                 FROM nix_packages",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?;
            let mut nix_count = 0usize;
            for row in rows.filter_map(|r| r.ok()) {
                let (attr, pname, version, description, long_description, homepage, license) = row;

                // Try to find a matching desktop file by pname or attr (case-insensitive).
                let desktop_key_pname = pname.to_lowercase();
                let desktop_key_attr = attr.to_lowercase();
                let desktop_match = desktop_files
                    .get(&desktop_key_pname)
                    .or_else(|| desktop_files.get(&desktop_key_attr));

                let (icon_name, desktop_ids) = if let Some(df) = desktop_match {
                    // Use the icon name from the desktop file's Icon= field if it
                    // actually exists in the theme; otherwise fall through to pname.
                    let icon = df
                        .icon_name
                        .as_deref()
                        .filter(|n| available_icons.contains(*n))
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            // Desktop file found but icon not in theme — try pname/attr.
                            if !pname.is_empty() && available_icons.contains(&pname) {
                                pname.clone()
                            } else if available_icons.contains(&attr) {
                                attr.clone()
                            } else {
                                "package-x-generic".to_string()
                            }
                        });
                    (icon, vec![df.desktop_stem.clone()])
                } else {
                    // No desktop file — pick icon from theme scan by pname/attr,
                    // fall back to generic so resolve_icon() short-circuits immediately.
                    let icon = if !pname.is_empty() && available_icons.contains(&pname) {
                        pname.clone()
                    } else if available_icons.contains(&attr) {
                        attr.clone()
                    } else {
                        "package-x-generic".to_string()
                    };
                    (icon, vec![])
                };

                let info = build_nix_info(
                    &attr, &pname, &version, &description, &long_description, &homepage,
                    &license, &icon_name, desktop_ids,
                );
                let id = AppId::new(&attr);
                self.infos.insert(id, Arc::new(info));
                nix_count += 1;
            }
            log::info!("catalog: loaded {} nix packages in {:?}", nix_count, t.elapsed());
        }

        // ── Flatpak apps (loaded second so they win on deduplication by slug) ─
        // The app_map table tells us which Flatpak IDs and Nix attrs share a slug.
        // For slugs with both sources, we load Flatpak as the primary AppInfo and
        // store the nix_attr in pkgnames so operation() can use it.
        log::info!("catalog: loading app_map...");
        let app_map: HashMap<String, (Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT flatpak_id, nix_attr FROM app_map WHERE flatpak_id IS NOT NULL",
            )?;
            let mut m = HashMap::new();
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })?;
            for row in rows.filter_map(|r| r.ok()) {
                if let (Some(fp_id), nix_attr) = row {
                    m.insert(fp_id.clone(), (Some(fp_id), nix_attr));
                }
            }
            m
        };

        {
            log::info!("catalog: loading flatpak_apps...");
            let t = std::time::Instant::now();
            let mut stmt = conn.prepare(
                "SELECT app_id, name, summary, description, version, developer, license,
                        homepage, bugtracker_url, donation_url, categories, icon_name,
                        icon_local_path, screenshots, content_rating, flatpak_ref,
                        releases_json, verified, monthly_downloads
                 FROM flatpak_apps",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,   // app_id
                    row.get::<_, String>(1)?,   // name
                    row.get::<_, String>(2)?,   // summary
                    row.get::<_, String>(3)?,   // description
                    row.get::<_, String>(4)?,   // version
                    row.get::<_, String>(5)?,   // developer
                    row.get::<_, String>(6)?,   // license
                    row.get::<_, String>(7)?,   // homepage
                    row.get::<_, String>(8)?,   // bugtracker_url
                    row.get::<_, String>(9)?,   // donation_url
                    row.get::<_, String>(10)?,  // categories
                    row.get::<_, String>(11)?,  // icon_name
                    row.get::<_, String>(12)?,  // icon_local_path
                    row.get::<_, String>(13)?,  // screenshots
                    row.get::<_, String>(14)?,  // content_rating
                    row.get::<_, String>(15)?,  // flatpak_ref
                    row.get::<_, String>(16)?,  // releases_json
                    row.get::<_, i64>(17)?,     // verified
                    row.get::<_, i64>(18)?,     // monthly_downloads
                ))
            })?;

            for row in rows.filter_map(|r| r.ok()) {
                let (
                    app_id, name, summary, description, _version, developer, license,
                    homepage, bugtracker_url, donation_url, categories,
                    icon_name, icon_local_path, screenshots, content_rating, flatpak_ref,
                    releases_json, verified, monthly_downloads,
                ) = row;

                let mut info = build_flatpak_info(
                    &app_id, &name, &summary, &description, &_version,
                    &developer, &license, &homepage, &bugtracker_url, &donation_url,
                    &categories, &icon_name, &icon_local_path, &screenshots,
                    &content_rating, &flatpak_ref, &releases_json,
                    verified != 0, monthly_downloads as u64,
                );

                // If this Flatpak app has a matching Nix attr via app_map, record it in pkgnames
                // so install logic can prefer Nix if requested.
                if let Some((_, Some(nix_attr))) = app_map.get(&app_id) {
                    info.pkgnames.push(nix_attr.clone());
                }

                let id = AppId::new(&app_id);
                self.infos.insert(id, Arc::new(info));
            }
            let flatpak_loaded = self.infos.values().filter(|i| i.source_id == "flathub").count();
            log::info!("catalog: loaded {} flatpak apps in {:?}", flatpak_loaded, t.elapsed());

            // Deduplication: remove Nix entries for which a Flatpak equivalent
            // exists in app_map.  Flatpak wins — its AppInfo already has the Nix
            // attr recorded in pkgnames so install logic can still target Nix if
            // the user explicitly requests it.
            let mut removed = 0usize;
            for (_, nix_attr) in app_map.values().filter_map(|(_, n)| n.as_ref().map(|n| ((), n))) {
                if self.infos.remove(&AppId::new(nix_attr)).is_some() {
                    removed += 1;
                }
            }
            if removed > 0 {
                log::info!("catalog: removed {} Nix duplicates (Flatpak entry retained)", removed);
            }
        }

        let flatpak_count = self.infos.values().filter(|i| i.source_id == "flathub").count();
        let nix_count = self.infos.len() - flatpak_count;
        if self.infos.is_empty() {
            log::warn!(
                "catalog: loaded 0 apps from {} — catalog DB may be empty or need update",
                self.db_path.display()
            );
        } else {
            log::info!(
                "catalog: loaded {} apps ({} Flatpak, {} Nix)",
                self.infos.len(),
                flatpak_count,
                nix_count
            );
        }
        Ok(())
    }

    fn info_caches(&self) -> &[AppstreamCache] {
        // The catalog backend does not use AppstreamCache; infos are exposed
        // directly via catalog_infos() instead.
        &[]
    }

    fn catalog_infos(&self) -> Option<&HashMap<AppId, Arc<AppInfo>>> {
        Some(&self.infos)
    }

    fn resolve_icon(&self, info: &AppInfo) -> Option<widget::icon::Handle> {
        for icon in &info.icons {
            match icon {
                AppIcon::Local(path, _, _, _) if !path.is_empty() => {
                    let p = Path::new(path);
                    if p.is_file() {
                        return Some(widget::icon::from_path(p.to_path_buf()));
                    }
                }
                AppIcon::Stock(name) if name != "package-x-generic" => {
                    return Some(
                        widget::icon::from_name(name.as_str())
                            .size(128)
                            .handle(),
                    );
                }
                _ => {}
            }
        }
        // Final fallback: generic package icon
        Some(
            widget::icon::from_name("package-x-generic")
                .size(128)
                .handle(),
        )
    }

    fn installed(&self) -> Result<Vec<Package>, Box<dyn Error>> {
        // Collect all installed app IDs from two sources:
        //   1. mirror-os list --json  (HM-managed apps: user-scope Flatpak + Nix)
        //   2. flatpak list --system  (system-scope Flatpaks from managed-apps.list)
        // Using a set prevents duplicates when an app is in both.
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut packages = Vec::new();

        // ── Source 1: mirror-os list --json ───────────────────────────────────
        if let Ok(output) = Command::new("mirror-os").args(["list", "--json"]).output() {
            if output.status.success() {
                let items: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
                    .unwrap_or_default();
                for item in items {
                    let source_id = item["source_id"].as_str().unwrap_or("").to_string();
                    if source_id.is_empty() || seen_ids.contains(&source_id) {
                        continue;
                    }
                    seen_ids.insert(source_id.clone());
                    let display_name = item["display_name"].as_str().unwrap_or("").to_string();
                    let version = item["version"].as_str().unwrap_or("").to_string();
                    let id = AppId::new(&source_id);
                    let info = if let Some(cached) = self.infos.get(&id) {
                        cached.clone()
                    } else {
                        Arc::new(AppInfo {
                            source_id: source_id.clone(),
                            source_name: item["source"].as_str().unwrap_or("").to_string(),
                            name: display_name,
                            icons: vec![AppIcon::Stock("package-x-generic".to_string())],
                            ..AppInfo::default()
                        })
                    };
                    let icon = self.resolve_icon(&info).unwrap_or_else(|| {
                        widget::icon::from_name("package-x-generic").size(128).handle()
                    });
                    packages.push(Package { id, icon, info, version, extra: HashMap::new() });
                }
            }
        }

        // ── Source 2: system-scope Flatpaks ───────────────────────────────────
        // These are managed-apps.list entries installed by mirror-flatpak-install.
        if let Ok(output) = Command::new("flatpak")
            .args(["list", "--system", "--app", "--columns=application"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let app_id = line.trim();
                    if app_id.is_empty() || seen_ids.contains(app_id) {
                        continue;
                    }
                    seen_ids.insert(app_id.to_string());
                    let id = AppId::new(app_id);
                    let info = if let Some(cached) = self.infos.get(&id) {
                        cached.clone()
                    } else {
                        Arc::new(AppInfo {
                            source_id: app_id.to_string(),
                            source_name: "flathub".to_string(),
                            icons: vec![AppIcon::Stock("package-x-generic".to_string())],
                            ..AppInfo::default()
                        })
                    };
                    let icon = self.resolve_icon(&info).unwrap_or_else(|| {
                        widget::icon::from_name("package-x-generic").size(128).handle()
                    });
                    packages.push(Package {
                        id,
                        icon,
                        info,
                        version: String::new(),
                        extra: HashMap::new(),
                    });
                }
            }
        }

        log::info!("catalog: {} installed apps detected", packages.len());
        Ok(packages)
    }

    fn updates(&self) -> Result<Vec<Package>, Box<dyn Error>> {
        // Updates are OS-level (bootc upgrade), not per-app
        Ok(Vec::new())
    }

    fn file_packages(&self, _path: &str) -> Result<Vec<Package>, Box<dyn Error>> {
        Ok(Vec::new())
    }

    fn operation(
        &self,
        op: &Operation,
        mut f: Box<dyn FnMut(f32) + 'static>,
    ) -> Result<(), Box<dyn Error>> {
        let Some(app_id) = op.package_ids.first() else {
            return Err("operation: no app_id provided".into());
        };
        let Some(info) = op.infos.first() else {
            return Err("operation: no AppInfo provided".into());
        };

        let source_flag = if info.source_id == "nixpkgs" { "--nix" } else { "--flatpak" };
        let raw_id = app_id.raw().to_string();

        let mut cmd = Command::new("mirror-os");
        // Ask trigger_switch to tee HM output to stdout so we can track progress
        cmd.env("MIRROR_OS_STREAM", "1");
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        match &op.kind {
            OperationKind::Install => {
                log::info!("catalog: installing {} ({})", raw_id, source_flag);
                cmd.args(["install", &raw_id, source_flag, "--yes"]);
            }
            OperationKind::Uninstall { .. } => {
                log::info!("catalog: uninstalling {}", raw_id);
                cmd.args(["uninstall", &raw_id, "--yes"]);
            }
            _ => {
                log::warn!("catalog: unsupported operation kind {:?}", op.kind);
                f(100.0);
                return Ok(());
            }
        }

        f(0.0);

        let mut child = cmd.spawn().map_err(|e| format!("mirror-os spawn: {}", e))?;
        let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
        let reader = BufReader::new(stdout);

        // Map known home-manager log lines to monotonically increasing progress values.
        // Each phase gate only advances progress; it never goes backwards.
        let mut progress: f32 = 0.0;
        let mut in_build_phase = false;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            log::debug!("mirror-os: {}", line);

            let candidate: Option<f32> = if line.contains("will be built:") || line.contains("these derivations will be built") {
                in_build_phase = true;
                Some(5.0)
            } else if line.contains("building '") || line.contains("building \"") {
                in_build_phase = true;
                // Asymptotically advance toward 65% (each build step covers 15% of remaining gap)
                Some((progress + (65.0 - progress) * 0.15).min(65.0))
            } else if line.contains("copying path") || line.contains("copying '") {
                if !in_build_phase {
                    in_build_phase = true;
                }
                Some(75.0_f32.max(progress))
            } else if line.contains("activating configuration") {
                Some(85.0_f32.max(progress))
            } else if line.starts_with("Activating ") {
                Some(90.0_f32.max(progress))
            } else if line.trim() == "Done." {
                Some(97.0)
            } else {
                None
            };

            if let Some(p) = candidate {
                if p > progress {
                    progress = p;
                    f(progress);
                }
            }
        }

        let status = child.wait().map_err(|e| format!("mirror-os wait: {}", e))?;
        if !status.success() {
            let op_name = match &op.kind {
                OperationKind::Install => "install",
                OperationKind::Uninstall { .. } => "uninstall",
                _ => "operation",
            };
            return Err(format!("mirror-os {} {} failed", op_name, raw_id).into());
        }

        f(100.0);
        Ok(())
    }
}

impl fmt::Display for CatalogBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CatalogBackend({})", self.db_path.display())
    }
}
