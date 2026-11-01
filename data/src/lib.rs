pub mod aggr;
pub mod audio;
pub mod chart;
pub mod config;
pub mod layout;
pub mod log;
pub mod metadata;
pub mod news;
pub mod orderflow;
pub mod panel;
pub mod stream;
pub mod tickers_table;
pub mod util;

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub use audio::AudioStream;
pub use config::ScaleFactor;
pub use config::network::{Network, TradeFetchMode};
pub use config::sidebar::{self, Sidebar};
pub use config::state::{CURRENT_SAVED_STATE_VERSION, Layouts, State};
pub use config::theme::Theme;
pub use config::timezone::UserTimezone;

use ::log::{error, info, warn};
pub use layout::{Dashboard, Layout, Pane};
pub use metadata::{MarketMetadata, MetadataSource};

pub const SAVED_STATE_PATH: &str = "saved-state.json";
const SAVED_STATE_BACKUP_PATH: &str = "saved-state.backup.json";
const SAVED_STATE_BACKUP_RETENTION: usize = 5;

#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    pub warnings: Vec<String>,
}

impl MigrationReport {
    fn new(from_version: u32) -> Self {
        Self {
            from_version,
            to_version: CURRENT_SAVED_STATE_VERSION,
            warnings: Vec::new(),
        }
    }

    pub fn migrated(&self) -> bool {
        self.from_version != self.to_version
    }

    pub fn recovered(&self) -> bool {
        !self.warnings.is_empty()
    }
}

#[derive(Clone)]
pub enum StateLoadOutcome {
    Loaded(State),
    Migrated {
        state: State,
        from_version: u32,
        to_version: u32,
        backup_path: Option<PathBuf>,
    },
    Recovered {
        state: State,
        warnings: Vec<String>,
        backup_path: Option<PathBuf>,
        save_path: PathBuf,
    },
    Corrupt {
        error: String,
        original_path: PathBuf,
        backup_path: Option<PathBuf>,
    },
    MissingDefault(State),
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum InternalError {
    #[error("Fetch error: {0}")]
    Fetch(String),
    #[error("Layout error: {0}")]
    Layout(String),
}

pub fn write_json_to_file(json: &str, file_name: &str) -> std::io::Result<()> {
    let path = data_path(Some(file_name));

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid state file path")
    })?;

    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
    }

    // Write to a temp file and rename so the previous good state survives a
    // crash or partial write.
    let tmp_path = path.with_extension("tmp");

    let write_result = File::create(&tmp_path).and_then(|mut file| {
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp_path, &path)
    });

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

pub fn read_from_file(file_name: &str) -> Result<State, Box<dyn std::error::Error>> {
    let path = data_path(Some(file_name));

    let file_open_result = File::open(&path);
    let mut file = match file_open_result {
        Ok(file) => file,
        Err(e) => return Err(Box::new(e)),
    };

    let mut contents = String::new();
    if let Err(e) = file.read_to_string(&mut contents) {
        return Err(Box::new(e));
    }

    match serde_json::from_str(&contents) {
        Ok(state) => Ok(state),
        Err(e) => {
            // If parsing fails, backup the file
            drop(file); // Close the file before renaming

            // Create backup file with different name to prevent overwriting it
            let backup_file_name = if let Some(pos) = file_name.rfind('.') {
                format!("{}_old{}", &file_name[..pos], &file_name[pos..])
            } else {
                format!("{}_old", file_name)
            };

            let backup_path = data_path(Some(&backup_file_name));

            if let Err(rename_err) = std::fs::rename(&path, &backup_path) {
                warn!(
                    "Failed to backup corrupted state file '{}' to '{}': {}",
                    path.display(),
                    backup_path.display(),
                    rename_err
                );
            } else {
                info!(
                    "Backed up corrupted state file to '{}'. It can be restored manually.",
                    backup_path.display()
                );
            }

            Err(Box::new(e))
        }
    }
}

pub fn load_saved_state_file() -> StateLoadOutcome {
    let path = data_path(Some(SAVED_STATE_PATH));
    load_saved_state_from_path(&path)
}

pub fn load_saved_state_from_path(path: &Path) -> StateLoadOutcome {
    load_saved_state_with_backup(path, backup_saved_state)
}

fn load_saved_state_with_backup(
    path: &Path,
    backup: impl Fn(&Path, &str) -> std::io::Result<PathBuf>,
) -> StateLoadOutcome {
    info!("SAVED_STATE LoadStart | path={}", path.display());

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            info!("SAVED_STATE Missing | action=default");
            return StateLoadOutcome::MissingDefault(State::default());
        }
        Err(err) => {
            error!("SAVED_STATE ReadFailed | error={err}");
            return StateLoadOutcome::Corrupt {
                error: err.to_string(),
                original_path: path.to_path_buf(),
                backup_path: backup(path, "corrupt").ok(),
            };
        }
    };

    let mut value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(err) => {
            error!("SAVED_STATE ParseFailed | error={err}");
            let backup_path = backup(path, "corrupt").ok();
            return StateLoadOutcome::Corrupt {
                error: err.to_string(),
                original_path: path.to_path_buf(),
                backup_path,
            };
        }
    };

    let report = migrate_saved_state_value(&mut value);
    if report.migrated() {
        info!(
            "SAVED_STATE MigrateStart | from_version={} to_version={}",
            report.from_version, report.to_version
        );
    }

    if report.from_version > CURRENT_SAVED_STATE_VERSION {
        let error = format!(
            "saved state version {} is newer than supported version {}",
            report.from_version, CURRENT_SAVED_STATE_VERSION
        );
        error!("SAVED_STATE Incompatible | error={error}");
        return StateLoadOutcome::Corrupt {
            error,
            original_path: path.to_path_buf(),
            backup_path: backup(path, "incompatible").ok(),
        };
    }

    let mut state: State = match serde_json::from_value(value.clone()) {
        Ok(state) => state,
        Err(err) => {
            error!("SAVED_STATE DeserializeFailed | error={err}");
            let backup_path = backup(path, "corrupt").ok();
            return StateLoadOutcome::Corrupt {
                error: err.to_string(),
                original_path: path.to_path_buf(),
                backup_path,
            };
        }
    };

    let migrated = report.migrated();
    let from_version = report.from_version;
    let to_version = report.to_version;
    state.saved_state_version = CURRENT_SAVED_STATE_VERSION;
    let mut warnings = report.warnings;
    // Serde accepts unknown struct fields silently. Detect fields that this
    // build cannot represent before a subsequent save could discard them.
    if let Ok(represented) = serde_json::to_value(&state) {
        collect_unrepresented_fields(&value, &represented, "$", &mut warnings);
    }
    sanitize_state(&mut state, &mut warnings);

    // A repaired model may have lost unsupported panes/indicators. Keep it in
    // a separate session file, including all subsequent application saves.
    if !warnings.is_empty() {
        let backup_path = match backup(path, "recovered") {
            Ok(backup) => backup,
            Err(error) => {
                return StateLoadOutcome::Corrupt {
                    error: format!("cannot protect original state before recovery: {error}"),
                    original_path: path.to_path_buf(),
                    backup_path: None,
                };
            }
        };
        let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let save_path = path.with_file_name(format!(
            "saved-state.session-{stamp}-{}.json",
            std::process::id()
        ));
        if let Err(error) = save_saved_state_at(&state, &save_path) {
            return StateLoadOutcome::Corrupt {
                error: format!("cannot save recovered session: {error}"),
                original_path: path.to_path_buf(),
                backup_path: Some(backup_path),
            };
        }
        warn!(
            "SAVED_STATE Recovered | original={} session={} warnings={}",
            path.display(),
            save_path.display(),
            warnings.join("; ")
        );
        return StateLoadOutcome::Recovered {
            state,
            warnings,
            backup_path: Some(backup_path),
            save_path,
        };
    }

    if migrated {
        let backup_path = match backup(path, "v0") {
            Ok(backup) => Some(backup),
            Err(error) => {
                return StateLoadOutcome::Corrupt {
                    error: format!("cannot protect original state before migration: {error}"),
                    original_path: path.to_path_buf(),
                    backup_path: None,
                };
            }
        };
        match serde_json::to_string(&state)
            .map_err(std::io::Error::other)
            .and_then(|json| write_json_to_file_atomic_at(&json, path))
        {
            Ok(()) => {
                info!(
                    "SAVED_STATE Migrated | from_version={} to_version={} backup={}",
                    from_version,
                    to_version,
                    backup_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "none".to_string())
                );
            }
            Err(err) => warn!("SAVED_STATE MigratedWriteFailed | error={err}"),
        }

        return StateLoadOutcome::Migrated {
            state,
            from_version,
            to_version,
            backup_path,
        };
    }

    StateLoadOutcome::Loaded(state)
}

pub fn save_saved_state_atomic(state: &State) -> std::io::Result<()> {
    let path = data_path(Some(SAVED_STATE_PATH));
    save_saved_state_at(state, &path)
}

pub fn save_saved_state_at(state: &State, path: &Path) -> std::io::Result<()> {
    let json = serde_json::to_string(state).map_err(std::io::Error::other)?;
    write_json_to_file_atomic_at(&json, path)
}

pub fn write_json_to_file_atomic_at(json: &str, path: &Path) -> std::io::Result<()> {
    write_json_to_file_with_replace(json, path, |from, to| std::fs::rename(from, to))
}

fn write_json_to_file_with_replace(
    json: &str,
    path: &Path,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    info!("SAVED_STATE SaveStart | path={}", path.display());
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid state file path")
    })?;

    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension("json.tmp");
    info!(
        "SAVED_STATE SaveAtomic | tmp={} final={}",
        tmp_path.display(),
        path.display()
    );

    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
    }

    if path.exists() {
        let rolling_backup = if path
            .file_name()
            .is_some_and(|name| name == SAVED_STATE_PATH)
        {
            parent.join(SAVED_STATE_BACKUP_PATH)
        } else {
            path.with_extension("backup.json")
        };
        std::fs::copy(path, &rolling_backup)?;
    }

    // rename replaces an existing file on Windows too. Never unlink the
    // destination first: if replacement fails, both the old state and the
    // fully flushed new state must remain available for recovery.
    replace(&tmp_path, path)?;

    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }

    cleanup_saved_state_backups(parent);
    info!("SAVED_STATE SaveComplete | path={}", path.display());
    Ok(())
}

fn collect_unrepresented_fields(
    input: &serde_json::Value,
    represented: &serde_json::Value,
    location: &str,
    warnings: &mut Vec<String>,
) {
    match (input, represented) {
        (serde_json::Value::Object(input), serde_json::Value::Object(output)) => {
            for (key, value) in input {
                let location = format!("{location}.{key}");
                if let Some(output) = output.get(key) {
                    collect_unrepresented_fields(value, output, &location, warnings);
                } else {
                    warnings.push(format!("unsupported saved field {location}"));
                }
            }
        }
        (serde_json::Value::Array(input), serde_json::Value::Array(output)) => {
            for (index, (input, output)) in input.iter().zip(output).enumerate() {
                collect_unrepresented_fields(
                    input,
                    output,
                    &format!("{location}[{index}]"),
                    warnings,
                );
            }
        }
        _ => {}
    }
}

fn backup_saved_state(path: &Path, reason: &str) -> std::io::Result<PathBuf> {
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "saved state file does not exist",
        ));
    }

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid state file path")
    })?;
    let backup_path = parent.join(format!(
        "saved-state.{reason}.{}-{}.json",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    ));
    let mut original = File::open(path)?;
    let mut copy = File::options()
        .write(true)
        .create_new(true)
        .open(&backup_path)?;
    std::io::copy(&mut original, &mut copy)?;
    copy.sync_all()?;
    info!("SAVED_STATE BackupCreated | path={}", backup_path.display());
    Ok(backup_path)
}

fn migrate_saved_state_value(value: &mut serde_json::Value) -> MigrationReport {
    let from_version = value
        .get("saved_state_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(0);
    let mut report = MigrationReport::new(from_version);

    if !value.is_object() {
        report
            .warnings
            .push("saved state root was not an object; using defaults".to_string());
        *value = serde_json::json!({});
    }

    let root = value
        .as_object_mut()
        .expect("root object after normalization");
    root.insert(
        "saved_state_version".to_string(),
        serde_json::json!(CURRENT_SAVED_STATE_VERSION),
    );

    if let Some(layout_manager) = root.get_mut("layout_manager") {
        sanitize_layout_manager(layout_manager, &mut report.warnings);
    }

    if let Some(sidebar) = root.get_mut("sidebar") {
        sanitize_sidebar_value(sidebar, &mut report.warnings);
    }

    report
}

fn sanitize_state(state: &mut State, warnings: &mut Vec<String>) {
    if state.layout_manager.layouts.is_empty() {
        warnings.push("saved state contained no layouts; added default layout".to_string());
        state.layout_manager.layouts.push(Layout::default());
        state.layout_manager.active_layout = Some("Default".to_string());
    }

    if let Some(active) = &state.layout_manager.active_layout
        && !state
            .layout_manager
            .layouts
            .iter()
            .any(|layout| layout.name == *active)
    {
        warnings.push(format!(
            "active layout '{active}' was missing; selected first available layout"
        ));
        state.layout_manager.active_layout = state
            .layout_manager
            .layouts
            .first()
            .map(|layout| layout.name.clone());
    }

    sanitize_main_window(state, warnings);
}

fn sanitize_main_window(state: &mut State, warnings: &mut Vec<String>) {
    const MAX_REASONABLE_WINDOW_COORD: f32 = 20_000.0;

    let Some(window) = state.main_window else {
        return;
    };

    let invalid_size = !window.width.is_finite()
        || !window.height.is_finite()
        || window.width <= 0.0
        || window.height <= 0.0;
    let invalid_position = !window.pos_x.is_finite()
        || !window.pos_y.is_finite()
        || window.pos_x.abs() > MAX_REASONABLE_WINDOW_COORD
        || window.pos_y.abs() > MAX_REASONABLE_WINDOW_COORD;

    if invalid_size || invalid_position {
        state.main_window = None;
        warnings
            .push("main window position was invalid or off-screen; opening centered".to_string());
    }
}

fn sanitize_layout_manager(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(layout_manager) = value.as_object_mut() else {
        warnings.push("layout manager was invalid; using default layout".to_string());
        *value =
            serde_json::json!({ "layouts": [default_layout_value()], "active_layout": "Default" });
        return;
    };

    match layout_manager
        .get_mut("layouts")
        .and_then(serde_json::Value::as_array_mut)
    {
        Some(layouts) => {
            for layout in layouts {
                sanitize_layout(layout, warnings);
            }
        }
        None => {
            warnings.push("layouts list was missing; added default layout".to_string());
            layout_manager.insert(
                "layouts".to_string(),
                serde_json::json!([default_layout_value()]),
            );
        }
    }
}

fn sanitize_layout(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(layout) = value.as_object_mut() else {
        warnings.push("layout entry was invalid; replaced with default layout".to_string());
        *value = default_layout_value();
        return;
    };

    if !layout.get("name").is_some_and(serde_json::Value::is_string) {
        layout.insert("name".to_string(), serde_json::json!("Recovered"));
    }

    let dashboard = layout
        .entry("dashboard")
        .or_insert_with(|| serde_json::json!({}));
    sanitize_dashboard(dashboard, warnings);
}

fn sanitize_dashboard(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(dashboard) = value.as_object_mut() else {
        warnings.push("dashboard was invalid; replaced with starter pane".to_string());
        *value = serde_json::json!({ "pane": default_pane_value(), "popout": [] });
        return;
    };

    sanitize_pane(
        dashboard.entry("pane").or_insert_with(default_pane_value),
        warnings,
    );

    match dashboard
        .get_mut("popout")
        .and_then(serde_json::Value::as_array_mut)
    {
        Some(popouts) => {
            popouts.retain_mut(|entry| {
                let Some(pair) = entry.as_array_mut() else {
                    warnings.push("dropped invalid popout entry".to_string());
                    return false;
                };
                if pair.len() != 2 {
                    warnings.push("dropped invalid popout tuple".to_string());
                    return false;
                }
                sanitize_pane(&mut pair[0], warnings);
                true
            });
        }
        None => {
            dashboard.insert("popout".to_string(), serde_json::json!([]));
        }
    }
}

fn sanitize_pane(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(object) = value.as_object_mut() else {
        warnings.push("pane was invalid; replaced with starter pane".to_string());
        *value = default_pane_value();
        return;
    };

    if object.len() != 1 {
        warnings.push("pane had invalid tagged form; replaced with starter pane".to_string());
        *value = default_pane_value();
        return;
    }

    let tag = object.keys().next().cloned().unwrap_or_default();
    match tag.as_str() {
        "Split" => {
            let Some(split) = object
                .get_mut("Split")
                .and_then(serde_json::Value::as_object_mut)
            else {
                warnings.push("split pane was invalid; replaced with starter pane".to_string());
                *value = default_pane_value();
                return;
            };
            if !matches!(
                split.get("axis").and_then(serde_json::Value::as_str),
                Some("Horizontal" | "Vertical")
            ) {
                split.insert("axis".to_string(), serde_json::json!("Horizontal"));
                warnings.push("split pane axis was invalid; defaulted to horizontal".to_string());
            }
            let ratio = split
                .get("ratio")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.5)
                .clamp(0.05, 0.95);
            split.insert("ratio".to_string(), serde_json::json!(ratio));
            sanitize_pane(
                split.entry("a").or_insert_with(default_pane_value),
                warnings,
            );
            sanitize_pane(
                split.entry("b").or_insert_with(default_pane_value),
                warnings,
            );
        }
        "Starter" => {}
        "NewsTracker" => {
            if let Some(pane) = object
                .get_mut("NewsTracker")
                .and_then(serde_json::Value::as_object_mut)
            {
                let config = pane
                    .entry("config")
                    .or_insert_with(|| serde_json::json!({}));
                if serde_json::from_value::<crate::news::Config>(config.clone()).is_err() {
                    *config = serde_json::json!({});
                    warnings.push("invalid News Tracker filters; reset to defaults".into());
                }
            }
        }
        "HeatmapChart" | "ShaderHeatmap" => {
            if let Some(pane) = object
                .get_mut(&tag)
                .and_then(serde_json::Value::as_object_mut)
            {
                sanitize_vec_tagged_enum(
                    pane.get_mut("indicators"),
                    &["Volume"],
                    warnings,
                    "heatmap indicator",
                );
                sanitize_streams(pane.get_mut("stream_type"), warnings);
                sanitize_settings(
                    pane.entry("settings")
                        .or_insert_with(|| serde_json::json!({})),
                    warnings,
                );
            }
        }
        "KlineChart" => {
            if let Some(pane) = object
                .get_mut("KlineChart")
                .and_then(serde_json::Value::as_object_mut)
            {
                sanitize_deserializable_vec::<crate::chart::indicator::KlineIndicator>(
                    pane.get_mut("indicators"),
                    warnings,
                    "unknown kline indicator",
                );
                sanitize_streams(pane.get_mut("stream_type"), warnings);
                sanitize_kline_kind(
                    pane.entry("kind")
                        .or_insert_with(|| serde_json::json!("Candles")),
                    warnings,
                );
                sanitize_settings(
                    pane.entry("settings")
                        .or_insert_with(|| serde_json::json!({})),
                    warnings,
                );
            }
        }
        "GexChart" => {
            if let Some(pane) = object
                .get_mut("GexChart")
                .and_then(serde_json::Value::as_object_mut)
            {
                if let Some(reference) = pane.get_mut("liquidity_reference")
                    && !reference.is_null()
                    && serde_json::from_value::<exchange::TickerInfo>(reference.clone()).is_err()
                {
                    *reference = serde_json::Value::Null;
                    warnings.push("dropped unsupported GEX liquidity reference".to_string());
                }
                sanitize_settings(
                    pane.entry("settings")
                        .or_insert_with(|| serde_json::json!({})),
                    warnings,
                );
            }
        }
        "ComparisonChart" | "TimeAndSales" | "Ladder" => {
            if let Some(pane) = object
                .get_mut(&tag)
                .and_then(serde_json::Value::as_object_mut)
            {
                sanitize_streams(pane.get_mut("stream_type"), warnings);
                sanitize_settings(
                    pane.entry("settings")
                        .or_insert_with(|| serde_json::json!({})),
                    warnings,
                );
            }
        }
        _ => {
            warnings.push(format!(
                "unknown pane type '{tag}' replaced with starter pane"
            ));
            *value = default_pane_value();
        }
    }
}

fn sanitize_settings(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(settings) = value.as_object_mut() else {
        warnings.push("pane settings were invalid; reset to defaults".to_string());
        *value = serde_json::json!({});
        return;
    };

    if let Some(visual_config) = settings.get_mut("visual_config")
        && !visual_config.is_null()
        && !is_known_tagged_enum(
            visual_config,
            &[
                "Heatmap",
                "TimeAndSales",
                "Kline",
                "Ladder",
                "Comparison",
                "Gex",
            ],
        )
    {
        settings.remove("visual_config");
        warnings.push("unknown visual config dropped".to_string());
    }

    if let Some(kline) = settings
        .get_mut("visual_config")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|config| config.get_mut("Kline"))
        .and_then(serde_json::Value::as_object_mut)
        && let Some(legacy) = kline.remove("gex_levels")
    {
        let indicator_configs = kline
            .entry("indicator_configs")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(indicator_configs) = indicator_configs.as_object_mut() {
            indicator_configs.entry("gex_levels").or_insert(legacy);
            warnings
                .push("legacy Kline gex_levels settings migrated to indicator_configs".to_string());
        }
    }
}

fn sanitize_kline_kind(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    if value.as_str() == Some("Candles") || is_known_tagged_enum(value, &["Footprint"]) {
        return;
    }

    *value = serde_json::json!("Candles");
    warnings.push("unknown kline chart kind defaulted to Candles".to_string());
}

fn sanitize_sidebar_value(value: &mut serde_json::Value, warnings: &mut Vec<String>) {
    let Some(sidebar) = value.as_object_mut() else {
        *value = serde_json::to_value(crate::Sidebar::default())
            .expect("default sidebar should serialize");
        warnings.push("sidebar was invalid; reset to defaults".to_string());
        return;
    };

    if !matches!(
        sidebar.get("position").and_then(serde_json::Value::as_str),
        Some("Left" | "Right")
    ) {
        sidebar.insert("position".to_string(), serde_json::json!("Left"));
        warnings.push("unknown sidebar position defaulted to left".to_string());
    }

    let Some(table_value) = sidebar.get_mut("tickers_table") else {
        return;
    };
    if table_value.is_null() {
        return;
    }

    let Some(table) = table_value.as_object_mut() else {
        *table_value = serde_json::Value::Null;
        warnings.push("ticker table settings were invalid; reset to defaults".to_string());
        return;
    };

    let defaults = serde_json::to_value(crate::tickers_table::Settings::default())
        .expect("default ticker table settings should serialize");
    for (key, default) in defaults
        .as_object()
        .expect("ticker table settings should serialize as an object")
    {
        table.entry(key.clone()).or_insert_with(|| default.clone());
    }

    sanitize_deserializable_vec::<exchange::Ticker>(
        table.get_mut("favorited_tickers"),
        warnings,
        "unsupported favorite ticker",
    );
    sanitize_deserializable_vec::<exchange::adapter::Venue>(
        table.get_mut("selected_exchanges"),
        warnings,
        "unsupported exchange",
    );
    sanitize_deserializable_vec::<exchange::adapter::MarketKind>(
        table.get_mut("selected_markets"),
        warnings,
        "unsupported market kind",
    );

    if serde_json::from_value::<crate::tickers_table::Settings>(table_value.clone()).is_err() {
        *table_value = serde_json::to_value(crate::tickers_table::Settings::default())
            .expect("default ticker table settings should serialize");
        warnings.push("ticker table settings were incompatible; reset to defaults".to_string());
    }
}

fn sanitize_streams(value: Option<&mut serde_json::Value>, warnings: &mut Vec<String>) {
    let Some(value) = value else {
        return;
    };
    let Some(streams) = value.as_array_mut() else {
        *value = serde_json::json!([]);
        warnings.push("stream list was invalid; reset to empty".to_string());
        return;
    };

    streams.retain(|entry| {
        let known_tag =
            is_known_tagged_enum(entry, &["Kline", "Depth", "Trades", "DepthAndTrades"]);
        let supported = known_tag
            && serde_json::from_value::<crate::stream::PersistStreamKind>(entry.clone()).is_ok();
        if !supported {
            warnings.push("dropped unsupported stream".to_string());
        }
        supported
    });
}

fn sanitize_deserializable_vec<T>(
    value: Option<&mut serde_json::Value>,
    warnings: &mut Vec<String>,
    warning: &str,
) where
    T: serde::de::DeserializeOwned,
{
    let Some(value) = value else {
        return;
    };
    let Some(entries) = value.as_array_mut() else {
        *value = serde_json::json!([]);
        warnings.push(format!("{warning} list was invalid; reset to empty"));
        return;
    };

    entries.retain(|entry| {
        let supported = serde_json::from_value::<T>(entry.clone()).is_ok();
        if !supported {
            warnings.push(format!("dropped {warning}"));
        }
        supported
    });
}

fn sanitize_vec_tagged_enum(
    value: Option<&mut serde_json::Value>,
    allowed: &[&str],
    warnings: &mut Vec<String>,
    label: &str,
) {
    let Some(value) = value else {
        return;
    };
    let Some(values) = value.as_array_mut() else {
        *value = serde_json::json!([]);
        warnings.push(format!("{label} list was invalid; reset to empty"));
        return;
    };

    values.retain(|entry| {
        let keep = entry.as_str().is_some_and(|tag| allowed.contains(&tag))
            || is_known_tagged_enum(entry, allowed);
        if !keep {
            warnings.push(format!("dropped unknown {label}"));
        }
        keep
    });
}

fn is_known_tagged_enum(value: &serde_json::Value, allowed: &[&str]) -> bool {
    if let Some(tag) = value.as_str() {
        return allowed.contains(&tag);
    }

    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == 1
        && object
            .keys()
            .next()
            .is_some_and(|tag| allowed.contains(&tag.as_str()))
}

fn default_layout_value() -> serde_json::Value {
    serde_json::json!({
        "name": "Default",
        "dashboard": {
            "pane": default_pane_value(),
            "popout": []
        }
    })
}

fn default_pane_value() -> serde_json::Value {
    serde_json::json!({ "Starter": { "link_group": null } })
}

fn cleanup_saved_state_backups(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };

    let mut backups = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            // Recovery, migration and session snapshots protect originals and
            // must not be aged out by ordinary saves. Rotate only autosnapshots.
            if name.starts_with("saved-state.auto-") && name.ends_with(".json") {
                Some((path, entry.metadata().and_then(|m| m.modified()).ok()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    backups.sort_by_key(|(_, modified)| *modified);
    let remove_count = backups.len().saturating_sub(SAVED_STATE_BACKUP_RETENTION);
    for (path, _) in backups.into_iter().take(remove_count) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn open_data_folder() -> Result<(), InternalError> {
    let pathbuf = data_path(None);

    if pathbuf.exists() {
        if let Err(err) = open::that(&pathbuf) {
            Err(InternalError::Layout(format!(
                "Failed to open data folder: {:?}, error: {}",
                pathbuf, err
            )))
        } else {
            info!("Opened data folder: {:?}", pathbuf);
            Ok(())
        }
    } else {
        Err(InternalError::Layout(format!(
            "Data folder does not exist: {:?}",
            pathbuf
        )))
    }
}

pub fn open_url(url: &str) -> Result<(), InternalError> {
    if let Err(err) = open::that(url) {
        Err(InternalError::Layout(format!(
            "Failed to open URL '{}': {}",
            url, err
        )))
    } else {
        info!("Opened URL: {url}");
        Ok(())
    }
}

pub fn data_path(path_name: Option<&str>) -> PathBuf {
    if let Ok(path) = std::env::var("FLOWSURFACE_DATA_PATH") {
        let base = PathBuf::from(path);
        if let Some(path_name) = path_name {
            base.join(path_name)
        } else {
            base
        }
    } else {
        let data_dir = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
        if let Some(path_name) = path_name {
            data_dir.join("flowsurface").join(path_name)
        } else {
            data_dir.join("flowsurface")
        }
    }
}

fn cleanup_directory(data_path: &PathBuf) -> usize {
    if !data_path.exists() {
        warn!("Data path {:?} does not exist, skipping cleanup", data_path);
        return 0;
    }

    let re =
        regex::Regex::new(r".*-(\d{4}-\d{2}-\d{2})\.zip$").expect("Cleanup regex pattern is valid");
    let today = chrono::Local::now().date_naive();
    let mut deleted_files = Vec::new();

    let entries = match std::fs::read_dir(data_path) {
        Ok(entries) => entries,
        Err(e) => {
            error!("Failed to read data directory {:?}: {}", data_path, e);
            return 0;
        }
    };

    for entry in entries.filter_map(Result::ok) {
        let symbol_dir = match std::fs::read_dir(entry.path()) {
            Ok(dir) => dir,
            Err(e) => {
                error!("Failed to read symbol directory {:?}: {}", entry.path(), e);
                continue;
            }
        };

        for file in symbol_dir.filter_map(Result::ok) {
            let path = file.path();
            let Some(filename) = path.to_str() else {
                continue;
            };

            if let Some(cap) = re.captures(filename)
                && let Ok(file_date) = chrono::NaiveDate::parse_from_str(&cap[1], "%Y-%m-%d")
            {
                let days_old = today.signed_duration_since(file_date).num_days();
                if days_old > 4 {
                    if let Err(e) = std::fs::remove_file(&path) {
                        error!("Failed to remove old file {}: {}", filename, e);
                    } else {
                        deleted_files.push(filename.to_string());
                        info!("Removed old file: {}", filename);
                    }
                }
            }
        }
    }

    deleted_files.len()
}

pub fn cleanup_old_market_data() -> usize {
    let paths = ["um", "cm"].map(|market_type| {
        data_path(Some(&format!(
            "market_data/binance/data/futures/{}/daily/aggTrades",
            market_type
        )))
    });

    let total_deleted: usize = paths.iter().map(cleanup_directory).sum();

    info!("File cleanup completed. Deleted {} files", total_deleted);
    total_deleted
}

#[cfg(test)]
mod tests {
    #[test]
    fn news_tracker_filters_survive_saved_state_sanitization() {
        let mut pane = serde_json::json!({"NewsTracker": {"config": {"assets": "BTC,ETH", "min_impact": 75, "sort_by_impact": true}}});
        let mut warnings = Vec::new();
        super::sanitize_pane(&mut pane, &mut warnings);
        assert!(warnings.is_empty());
        let super::Pane::NewsTracker { config } = serde_json::from_value(pane).unwrap() else {
            panic!("expected News Tracker")
        };
        assert_eq!(config.assets, "BTC,ETH");
        assert_eq!(config.min_impact, 75);
        assert!(config.sort_by_impact);
        let mut malformed = serde_json::json!({"NewsTracker":{"config":{"min_impact":"bad"}}});
        super::sanitize_pane(&mut malformed, &mut warnings);
        assert!(serde_json::from_value::<super::Pane>(malformed).is_ok());
        assert_eq!(warnings.len(), 1);
    }

    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_state_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "flowsurface-saved-state-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create test data dir");
        dir.join(name)
    }

    fn valid_state() -> State {
        let mut state = State::default();
        state.layout_manager.layouts.push(Layout::default());
        state.layout_manager.active_layout = Some("Default".to_string());
        state
    }

    #[test]
    fn missing_saved_state_returns_default() {
        let path = temp_state_path("missing.json");

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::MissingDefault(state) => {
                assert_eq!(state.saved_state_version, CURRENT_SAVED_STATE_VERSION);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn valid_current_saved_state_loads_cleanly() {
        let path = temp_state_path("saved-state.json");
        let state = valid_state();
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(loaded) => {
                assert_eq!(loaded.saved_state_version, CURRENT_SAVED_STATE_VERSION);
                assert_eq!(loaded.layout_manager.layouts.len(), 1);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn unknown_fields_are_preserved_in_original_and_recovery_backup() {
        let path = temp_state_path("saved-state.json");
        let mut value = serde_json::to_value(valid_state()).unwrap();
        value["future_feature"] = serde_json::json!({"enabled": true});
        value["layout_manager"]["layouts"][0]["dashboard"]["future_panel_option"] =
            serde_json::json!(42);
        let original = serde_json::to_vec_pretty(&value).unwrap();
        std::fs::write(&path, &original).unwrap();
        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                state,
                warnings,
                backup_path,
                save_path,
            } => {
                assert!(warnings.iter().any(|w| w.contains("$.future_feature")));
                assert!(warnings.iter().any(|w| w.contains("future_panel_option")));
                assert_eq!(std::fs::read(backup_path.unwrap()).unwrap(), original);
                save_saved_state_at(&state, &save_path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            _ => panic!("unknown fields require a separate recovery session"),
        }
    }

    #[test]
    fn failed_backup_blocks_recovery_and_migration_without_overwrite() {
        for legacy in [false, true] {
            let path = temp_state_path("saved-state.json");
            let mut value = serde_json::to_value(valid_state()).unwrap();
            if legacy {
                value.as_object_mut().unwrap().remove("saved_state_version");
            } else {
                value["layout_manager"]["layouts"][0]["dashboard"]["pane"] =
                    serde_json::json!({"FuturePane": {}});
            }
            let original = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &original).unwrap();
            let outcome = load_saved_state_with_backup(&path, |_, _| {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            });
            assert!(matches!(
                outcome,
                StateLoadOutcome::Corrupt {
                    backup_path: None,
                    ..
                }
            ));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                1
            );
        }
    }

    #[test]
    fn recovery_snapshots_and_session_files_survive_normal_save_rotation() {
        let path = temp_state_path("saved-state.json");
        let parent = path.parent().unwrap();
        for index in 0..10 {
            std::fs::write(
                parent.join(format!("saved-state.auto-{index}.json")),
                "auto",
            )
            .unwrap();
        }
        for name in [
            "saved-state.recovered.keep.json",
            "saved-state.v0.keep.json",
            "saved-state.incompatible.keep.json",
            "saved-state.session-keep.json",
            "saved-state.session-keep.backup.json",
        ] {
            std::fs::write(parent.join(name), "protected").unwrap();
        }
        save_saved_state_at(&valid_state(), &path).unwrap();
        for name in [
            "saved-state.recovered.keep.json",
            "saved-state.v0.keep.json",
            "saved-state.incompatible.keep.json",
            "saved-state.session-keep.json",
            "saved-state.session-keep.backup.json",
        ] {
            assert_eq!(
                std::fs::read_to_string(parent.join(name)).unwrap(),
                "protected"
            );
        }
        assert_eq!(
            std::fs::read_dir(parent)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("saved-state.auto-"))
                .count(),
            SAVED_STATE_BACKUP_RETENTION
        );
    }

    #[test]
    fn recovery_session_saves_do_not_replace_original_rolling_backup() {
        let path = temp_state_path("saved-state.session-test.json");
        let rolling = path.parent().unwrap().join(SAVED_STATE_BACKUP_PATH);
        std::fs::write(&rolling, "original backup").unwrap();
        std::fs::write(&path, "previous session").unwrap();
        save_saved_state_at(&valid_state(), &path).unwrap();
        assert_eq!(std::fs::read_to_string(rolling).unwrap(), "original backup");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("backup.json")).unwrap(),
            "previous session"
        );
    }

    #[test]
    fn legacy_saved_state_without_version_is_migrated() {
        let path = temp_state_path("saved-state.json");
        let mut value = serde_json::to_value(valid_state()).unwrap();
        value.as_object_mut().unwrap().remove("saved_state_version");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Migrated {
                state,
                from_version,
                to_version,
                backup_path,
            } => {
                assert_eq!(from_version, 0);
                assert_eq!(to_version, CURRENT_SAVED_STATE_VERSION);
                assert_eq!(state.saved_state_version, CURRENT_SAVED_STATE_VERSION);
                assert!(backup_path.is_some_and(|path| path.exists()));
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn missing_current_fields_use_defaults() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": { "Starter": {} }
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(state) => {
                assert_eq!(state.layout_manager.layouts.len(), 1);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn unknown_pane_enum_recovers_without_panic() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": { "RemovedPaneType": { "old": true } },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                state, warnings, ..
            } => {
                assert_eq!(state.layout_manager.layouts.len(), 1);
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("unknown pane"))
                );
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn unknown_indicator_is_dropped_without_panic() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "KlineChart": {
                                "layout": { "splits": [], "autoscale": null },
                                "kind": "Candles",
                                "stream_type": [],
                                "settings": {},
                                "indicators": ["Volume", "RemovedIndicator"],
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered { warnings, .. } => {
                assert!(warnings.iter().any(|warning| warning.contains("indicator")));
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn unsupported_exchange_data_is_dropped_without_losing_valid_layout_configuration() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [
                    {
                        "name": "BTCUSDT",
                        "dashboard": {
                            "pane": {
                                "KlineChart": {
                                    "layout": { "splits": [0.8], "autoscale": "FitToVisible" },
                                    "kind": "Candles",
                                    "drawings": [],
                                    "stream_type": [{
                                        "Kline": {
                                            "ticker": "BinanceLinear:BTCUSDT",
                                            "timeframe": "M5"
                                        }
                                    }],
                                    "settings": {
                                        "visual_config": {
                                            "Kline": { "data_labels_always_visible": true }
                                        },
                                        "selected_basis": { "Time": "M5" }
                                    },
                                    "indicators": ["VolumeBubbles", "GexLevels"],
                                    "link_group": null
                                }
                            },
                            "popout": []
                        }
                    },
                    {
                        "name": "Unsupported futures",
                        "dashboard": {
                            "pane": {
                                "KlineChart": {
                                    "layout": { "splits": [], "autoscale": null },
                                    "kind": "Candles",
                                    "drawings": [],
                                    "stream_type": [
                                        {
                                            "Kline": {
                                                "ticker": "RithmicFutures:CME-Delayed/NQU6",
                                                "timeframe": "M15"
                                            }
                                        },
                                        {
                                            "Trades": {
                                                "ticker": "RithmicFutures:CME-Delayed/NQU6"
                                            }
                                        }
                                    ],
                                    "settings": {},
                                    "indicators": ["Volume"],
                                    "link_group": null
                                }
                            },
                            "popout": []
                        }
                    }
                ],
                "active_layout": "BTCUSDT"
            },
            "sidebar": {
                "position": "Left",
                "tickers_table": {
                    "favorited_tickers": [
                        "BinanceLinear:BTCUSDT",
                        "RithmicFutures:CME-Delayed/NQU6"
                    ],
                    "show_favorites": false,
                    "selected_sort_option": "VolumeDesc",
                    "selected_exchanges": ["Binance", "Rithmic"],
                    "selected_markets": ["LinearPerps", "Futures"]
                }
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                state, warnings, ..
            } => {
                assert_eq!(state.layout_manager.layouts.len(), 2);
                assert!(warnings.iter().any(|warning| warning.contains("exchange")));
                assert!(warnings.iter().any(|warning| warning.contains("stream")));

                let crate::Pane::KlineChart {
                    settings,
                    indicators,
                    stream_type,
                    ..
                } = &state.layout_manager.layouts[0].dashboard.pane
                else {
                    panic!("expected valid KlineChart pane");
                };
                assert_eq!(stream_type.len(), 1);
                assert!(settings.visual_config.is_some());
                assert!(
                    indicators.contains(&crate::chart::indicator::KlineIndicator::VolumeBubbles)
                );
                assert!(indicators.contains(&crate::chart::indicator::KlineIndicator::GexLevels));

                let crate::Pane::KlineChart { stream_type, .. } =
                    &state.layout_manager.layouts[1].dashboard.pane
                else {
                    panic!("expected unsupported KlineChart shell to be preserved");
                };
                assert!(stream_type.is_empty());

                let ticker_settings = state
                    .sidebar
                    .tickers_table
                    .expect("ticker settings should be preserved");
                assert_eq!(
                    ticker_settings.selected_exchanges,
                    vec![exchange::adapter::Venue::Binance]
                );
                assert_eq!(ticker_settings.favorited_tickers.len(), 1);
            }
            _ => panic!("expected recovered state"),
        }
    }

    #[test]
    fn removing_smc_and_legacy_cvd_controls_preserves_other_chart_settings() {
        use crate::chart::{indicator::KlineIndicator, kline::Config};
        let mut pane = serde_json::json!({"KlineChart":{
            "layout":{"splits":[0.75],"autoscale":"FitToVisible"},"kind":"Candles","drawings":[],
            "stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M5"}}],
            "settings":{"selected_basis":{"Time":"M5"},"visual_config":{"Kline":{
                "data_labels_always_visible":true,"smc":{"swing_length":40},
                "cvd":{"render_style":"Candlesticks","reset":"DailyUtc","show_wicks":true,"line_width":2.0},
                "scripted":{"rsi_period":9}
            }}},"indicators":["Volume","SmartMoney","CumulativeDelta","Rsi"],"link_group":null
        }});
        let mut warnings = Vec::new();
        sanitize_pane(&mut pane, &mut warnings);
        assert_eq!(warnings, vec!["dropped unknown kline indicator"]);
        let crate::Pane::KlineChart {
            indicators,
            settings,
            stream_type,
            layout,
            ..
        } = serde_json::from_value(pane).unwrap()
        else {
            panic!("chart lost")
        };
        assert_eq!(
            indicators,
            vec![
                KlineIndicator::Volume,
                KlineIndicator::CumulativeDelta,
                KlineIndicator::Rsi
            ]
        );
        assert_eq!(layout.splits, vec![0.75]);
        assert_eq!(stream_type.len(), 1);
        let Some(crate::layout::pane::VisualConfig::Kline(config)) = settings.visual_config else {
            panic!("chart configuration lost")
        };
        assert!(config.data_labels_always_visible);
        assert_eq!(config.scripted.rsi_period, 9);
        assert!(!config.cvd.show_change_bars);
        let saved = serde_json::to_value(config).unwrap();
        assert!(saved.get("smc").is_none());
        assert!(saved["cvd"].get("reset").is_none());
        assert_eq!(serde_json::from_value::<Config>(saved).unwrap(), config);
    }

    #[test]
    fn all_supported_kline_indicators_survive_recovery() {
        use crate::chart::indicator::{Indicator, KlineIndicator};
        let supported = KlineIndicator::for_market(exchange::adapter::MarketKind::LinearPerps);
        let mut indicators = serde_json::to_value(supported).unwrap();
        indicators
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("FutureIndicator"));
        let mut pane = serde_json::json!({"KlineChart": {
            "layout": {"splits": [], "autoscale": null},
            "kind": "Candles", "drawings": [], "stream_type": [],
            "settings": {}, "indicators": indicators, "link_group": null
        }});
        let mut warnings = Vec::new();
        sanitize_pane(&mut pane, &mut warnings);
        assert_eq!(warnings, vec!["dropped unknown kline indicator"]);
        let crate::Pane::KlineChart { indicators, .. } = serde_json::from_value(pane).unwrap()
        else {
            panic!("supported chart must survive recovery");
        };
        assert_eq!(indicators, supported);
    }

    #[test]
    fn overlay_indicators_are_preserved_through_sanitize() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "KlineChart": {
                                "layout": { "splits": [], "autoscale": null },
                                "kind": "Candles",
                                "stream_type": [],
                                "settings": {},
                                "indicators": ["Volume", "VolumeBubbles", "SessionVolumeProfile", "Vwap", "GexLevels", "CumulativeDelta"],
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(state) => {
                let pane = &state.layout_manager.layouts[0].dashboard.pane;
                if let crate::Pane::KlineChart { indicators, .. } = pane {
                    assert_eq!(
                        indicators.len(),
                        6,
                        "all overlay indicators should be preserved"
                    );
                    assert!(indicators.contains(&crate::chart::indicator::KlineIndicator::Volume));
                    assert!(
                        indicators
                            .contains(&crate::chart::indicator::KlineIndicator::VolumeBubbles)
                    );
                    assert!(
                        indicators.contains(
                            &crate::chart::indicator::KlineIndicator::SessionVolumeProfile
                        )
                    );
                    assert!(indicators.contains(&crate::chart::indicator::KlineIndicator::Vwap));
                    assert!(
                        indicators.contains(&crate::chart::indicator::KlineIndicator::GexLevels)
                    );
                    assert!(
                        indicators
                            .contains(&crate::chart::indicator::KlineIndicator::CumulativeDelta)
                    );
                } else {
                    panic!("expected KlineChart pane");
                }
            }
            _ => panic!("expected Loaded outcome"),
        }
    }

    #[test]
    fn gex_pane_and_visual_config_are_preserved_through_sanitize() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "GexChart": {
                                "underlying": "Btc",
                                "settings": {
                                    "visual_config": {
                                        "Gex": {
                                            "expiry_filter": "SevenDays",
                                            "price_range_percent": 20.0
                                        }
                                    }
                                },
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(state) => {
                let pane = &state.layout_manager.layouts[0].dashboard.pane;
                let crate::Pane::GexChart { settings, .. } = pane else {
                    panic!("expected GexChart pane");
                };
                let config = settings
                    .visual_config
                    .as_ref()
                    .and_then(crate::layout::pane::VisualConfig::gex)
                    .expect("GEX visual config");
                assert_eq!(config.price_range_percent, 20.0);
                assert_eq!(config.max_visible_strikes, 40);
            }
            _ => panic!("expected Loaded outcome"),
        }
    }

    #[test]
    fn legacy_kline_gex_levels_are_migrated_to_indicator_config() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "KlineChart": {
                                "layout": { "splits": [], "autoscale": null },
                                "kind": "Candles",
                                "stream_type": [],
                                "settings": {
                                    "visual_config": {
                                        "Kline": {
                                            "gex_levels": {
                                                "expiry_filter": "ThirtyDays",
                                                "max_clusters": 7,
                                                "basis_mode": "ShiftToChartPrice"
                                            }
                                        }
                                    }
                                },
                                "indicators": ["GexLevels"],
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                state, warnings, ..
            } => {
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("gex_levels"))
                );
                let crate::Pane::KlineChart { settings, .. } =
                    &state.layout_manager.layouts[0].dashboard.pane
                else {
                    panic!("expected KlineChart pane");
                };
                let config = settings
                    .visual_config
                    .as_ref()
                    .and_then(crate::layout::pane::VisualConfig::kline)
                    .expect("Kline visual config");
                let levels = config.gex_levels();
                assert_eq!(
                    levels.expiry_filter,
                    crate::chart::gex::GexExpiryFilter::ThirtyDays
                );
                assert_eq!(levels.current_profile_width_percent, 5.0);

                let serialized = serde_json::to_value(config).unwrap();
                assert!(serialized.get("gex_levels").is_none());
                assert!(
                    serialized["indicator_configs"]["gex_levels"]
                        .get("max_clusters")
                        .is_none()
                );
            }
            _ => panic!("expected recovered state with migration warning"),
        }
    }

    #[test]
    fn null_visual_config_loads_cleanly() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "Ladder": {
                                "stream_type": [],
                                "settings": {
                                    "tick_multiply": 200,
                                    "visual_config": null,
                                    "selected_basis": null
                                },
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(state) => {
                assert_eq!(state.layout_manager.layouts.len(), 1);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn recovered_saved_state_is_saved_separately_and_original_survives_saves() {
        let path = temp_state_path("saved-state.json");
        let value = serde_json::json!({
            "saved_state_version": CURRENT_SAVED_STATE_VERSION,
            "layout_manager": {
                "layouts": [{
                    "name": "Default",
                    "dashboard": {
                        "pane": {
                            "KlineChart": {
                                "layout": { "splits": [], "autoscale": null },
                                "kind": "Candles",
                                "stream_type": [],
                                "settings": {
                                    "visual_config": { "RemovedConfig": {} }
                                },
                                "indicators": [],
                                "link_group": null
                            }
                        },
                        "popout": []
                    }
                }],
                "active_layout": "Default"
            }
        });
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        let original = std::fs::read(&path).unwrap();
        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                warnings,
                state,
                save_path,
                backup_path,
            } => {
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("visual config"))
                );
                assert_ne!(save_path, path);
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(std::fs::read(backup_path.unwrap()).unwrap(), original);
                save_saved_state_at(&state, &save_path).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert!(matches!(
                    load_saved_state_from_path(&save_path),
                    StateLoadOutcome::Loaded(_)
                ));
            }
            _ => panic!("unexpected outcome"),
        }

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered { state, .. } => {
                assert_eq!(state.layout_manager.layouts.len(), 1);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn offscreen_main_window_position_recovers_to_centered_startup() {
        let path = temp_state_path("saved-state.json");
        let mut state = valid_state();
        state.main_window = Some(layout::Window {
            width: 800.0,
            height: 600.0,
            pos_x: -25_600.0,
            pos_y: -25_600.0,
        });
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered {
                state, warnings, ..
            } => {
                assert!(state.main_window.is_none());
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("off-screen"))
                );
            }
            _ => panic!("unexpected outcome"),
        }

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Recovered { state, .. } => {
                assert!(state.main_window.is_none());
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn nearby_negative_main_window_position_is_preserved() {
        let path = temp_state_path("saved-state.json");
        let mut state = valid_state();
        state.main_window = Some(layout::Window {
            width: 800.0,
            height: 600.0,
            pos_x: 1912.0,
            pos_y: -8.0,
        });
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Loaded(state) => {
                let window = state.main_window.expect("main window should be preserved");
                assert_eq!(window.pos_x, 1912.0);
                assert_eq!(window.pos_y, -8.0);
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn invalid_json_is_corrupt_and_backed_up_without_overwrite() {
        let path = temp_state_path("saved-state.json");
        std::fs::write(&path, "{ not json").unwrap();

        match load_saved_state_from_path(&path) {
            StateLoadOutcome::Corrupt {
                backup_path,
                original_path,
                ..
            } => {
                assert_eq!(original_path, path);
                assert_eq!(
                    std::fs::read_to_string(&original_path).unwrap(),
                    "{ not json"
                );
                assert!(backup_path.is_some_and(|path| path.exists()));
            }
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn atomic_save_creates_final_file_and_rolling_backup() {
        let path = temp_state_path("saved-state.json");
        std::fs::write(&path, "previous").unwrap();
        let json = serde_json::to_string(&valid_state()).unwrap();

        write_json_to_file_atomic_at(&json, &path).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), json);
        assert_eq!(
            std::fs::read_to_string(path.parent().unwrap().join(SAVED_STATE_BACKUP_PATH)).unwrap(),
            "previous"
        );
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn failed_state_replacement_retains_previous_and_pending_state() {
        let path = temp_state_path("saved-state.json");
        let previous = serde_json::to_string(&valid_state()).unwrap();
        std::fs::write(&path, &previous).unwrap();
        let mut changed = valid_state();
        changed.layout_manager.active_layout = Some("Changed".into());
        let pending = serde_json::to_string(&changed).unwrap();

        let error = write_json_to_file_with_replace(&pending, &path, |tmp, target| {
            assert_eq!(std::fs::read_to_string(target).unwrap(), previous);
            assert_eq!(std::fs::read_to_string(tmp).unwrap(), pending);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), previous);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.tmp")).unwrap(),
            pending
        );
        assert_eq!(
            std::fs::read_to_string(path.parent().unwrap().join(SAVED_STATE_BACKUP_PATH)).unwrap(),
            previous
        );
        write_json_to_file_atomic_at(&pending, &path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), pending);
    }
}
