//! config.json 위치·생성·검증·병합 → Settings (spec §3, 위치는 CLAUDE.md "다른 점").
//!
//! 설정 파일은 이 모듈에서만 읽고 쓴다. 다른 모듈은 Settings만 받는다.

use serde_json::{Map, Value, json};
use std::fmt;
use std::path::{Path, PathBuf};

pub const CONFIG_VERSION: i64 = 1;
const FORBIDDEN_FILENAME_CHARS: &str = "<>:\"/\\|?*";
/// Python 버전에만 있던 키. 같은 설정 파일을 써도 경고하지 않고 무시한다.
const IGNORED_KEYS: [&str; 1] = ["ffmpeg_dir"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Ko,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Webp,
    Jpg,
    Png,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Webp => "webp",
            OutputFormat::Jpg => "jpg",
            OutputFormat::Png => "png",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    Fast,
    Accurate,
}

/// 칸 안 타임스탬프 위치 (Rust 버전에만 있는 설정).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampPosition {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
    Center,
}

const TIMESTAMP_POSITIONS: [(&str, TimestampPosition); 5] = [
    ("bottom-right", TimestampPosition::BottomRight),
    ("bottom-left", TimestampPosition::BottomLeft),
    ("top-right", TimestampPosition::TopRight),
    ("top-left", TimestampPosition::TopLeft),
    ("center", TimestampPosition::Center),
];

impl TimestampPosition {
    pub fn name(self) -> &'static str {
        TIMESTAMP_POSITIONS
            .iter()
            .find(|(_, p)| *p == self)
            .map_or("bottom-right", |(n, _)| n)
    }

    fn from_name(name: &str) -> Self {
        TIMESTAMP_POSITIONS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(TimestampPosition::BottomRight, |(_, p)| *p)
    }
}

/// 헤더 항목별 표시 여부 (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderSettings {
    pub file_name: bool,
    pub size: bool,
    pub resolution: bool,
    pub fps: bool,
    pub video_codec: bool,
    pub audio_codec: bool,
    pub duration: bool,
}

const HEADER_KEYS: [&str; 7] = [
    "file_name",
    "size",
    "resolution",
    "fps",
    "video_codec",
    "audio_codec",
    "duration",
];

impl HeaderSettings {
    pub const fn all(on: bool) -> Self {
        HeaderSettings {
            file_name: on,
            size: on,
            resolution: on,
            fps: on,
            video_codec: on,
            audio_codec: on,
            duration: on,
        }
    }

    fn set(&mut self, key: &str, on: bool) {
        match key {
            "file_name" => self.file_name = on,
            "size" => self.size = on,
            "resolution" => self.resolution = on,
            "fps" => self.fps = on,
            "video_codec" => self.video_codec = on,
            "audio_codec" => self.audio_codec = on,
            "duration" => self.duration = on,
            _ => {}
        }
    }
}

impl Default for HeaderSettings {
    fn default() -> Self {
        Self::all(true)
    }
}

/// config.json 값 (spec §3.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub config_version: i64,
    pub columns: i64,
    pub rows: i64,
    pub width: i64,
    pub gap: i64,
    pub theme: ThemeName,
    pub language: Language,
    pub header: HeaderSettings,
    pub format: OutputFormat,
    pub quality: i64,
    pub suffix: String,
    pub output_dir: String,
    pub overwrite: bool,
    pub show_timestamp: bool,
    pub timestamp_position: TimestampPosition,
    pub recursive: bool,
    pub skip_start_seconds: f64,
    pub skip_end_seconds: f64,
    pub seek_mode: SeekMode,
    pub font: String,
    pub close_delay_seconds: i64,
}

impl Default for Settings {
    fn default() -> Self {
        settings_from(&defaults_map())
    }
}

/// 기본 설정을 파일에 쓸 모양으로 (키 순서가 파일의 키 순서다).
pub fn defaults_map() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "config_version": CONFIG_VERSION,
        "columns": 4,
        "rows": 5,
        "width": 1024,
        "gap": 8,
        "theme": "light",
        "language": "en",
        "header": {
            "file_name": true,
            "size": true,
            "resolution": true,
            "fps": true,
            "video_codec": true,
            "audio_codec": true,
            "duration": true
        },
        "format": "webp",
        "quality": 85,
        "suffix": "_sheet",
        "output_dir": "",
        "overwrite": false,
        "show_timestamp": true,
        "timestamp_position": "bottom-right",
        "recursive": false,
        "skip_start_seconds": 0,
        "skip_end_seconds": 0,
        "seek_mode": "fast",
        "font": "",
        "close_delay_seconds": 3
    }) else {
        unreachable!()
    };
    map
}

#[derive(Debug, PartialEq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug)]
pub struct LoadedConfig {
    pub settings: Settings,
    pub path: PathBuf,
    pub created: bool, // 이번에 새로 만들었는지
    pub warnings: Vec<String>,
}

// --- 값 검증 ---------------------------------------------------------------

fn as_int(v: &Value) -> Option<i64> {
    // JSON 정수만 (4.0, true는 정수가 아니다)
    if v.is_i64() || v.is_u64() {
        v.as_i64()
    } else {
        None
    }
}

fn check(key: &str, v: &Value) -> Option<String> {
    let int_range = |lo: i64, hi: i64| match as_int(v) {
        Some(n) if (lo..=hi).contains(&n) => None,
        _ => Some(format!("{lo}~{hi} 사이 정수여야 합니다")),
    };
    let choice = |options: &[&str]| match v.as_str() {
        Some(s) if options.contains(&s) => None,
        _ => Some(
            options
                .iter()
                .map(|o| format!("\"{o}\""))
                .collect::<Vec<_>>()
                .join(" / ")
                + " 중 하나여야 합니다",
        ),
    };
    let boolean = || (!v.is_boolean()).then(|| "true 또는 false여야 합니다".to_string());
    let string = || (!v.is_string()).then(|| "문자열(\"...\")이어야 합니다".to_string());
    match key {
        "config_version" | "close_delay_seconds" => {
            as_int(v).is_none().then(|| "정수여야 합니다".to_string())
        }
        "columns" | "rows" => int_range(1, 20),
        "width" => int_range(320, 8000),
        "gap" => int_range(0, 100),
        "quality" => int_range(1, 100),
        "theme" => choice(&["light", "dark"]),
        "language" => choice(&["en", "ko"]),
        "format" => choice(&["webp", "jpg", "png"]),
        "seek_mode" => choice(&["fast", "accurate"]),
        "timestamp_position" => choice(&TIMESTAMP_POSITIONS.map(|(n, _)| n)),
        "output_dir" | "font" => string(),
        "overwrite" | "show_timestamp" | "recursive" => boolean(),
        "skip_start_seconds" | "skip_end_seconds" => match v.as_f64() {
            Some(n) if n >= 0.0 => None,
            _ => Some("0 이상의 숫자여야 합니다".to_string()),
        },
        "suffix" => match v.as_str() {
            Some(s) if !s.is_empty() => {
                let mut bad: Vec<char> = s
                    .chars()
                    .filter(|c| FORBIDDEN_FILENAME_CHARS.contains(*c))
                    .collect();
                bad.sort_unstable();
                bad.dedup();
                (!bad.is_empty()).then(|| {
                    let list: Vec<String> = bad.iter().map(char::to_string).collect();
                    format!("파일명에 쓸 수 없는 문자가 있습니다: {}", list.join(" "))
                })
            }
            _ => Some("비어 있지 않은 문자열이어야 합니다".to_string()),
        },
        _ => None,
    }
}

fn is_known(key: &str) -> bool {
    key != "header" && defaults_map().contains_key(key)
}

fn show(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// 키별 오류 목록 ("키: 이유 (현재: 값)").
fn validate(data: &Map<String, Value>) -> Vec<String> {
    let mut errors = Vec::new();
    for key in defaults_map().keys() {
        if let Some(v) = data.get(key)
            && let Some(reason) = check(key, v)
        {
            errors.push(format!("{key}: {reason} (현재: {})", show(v)));
        }
    }
    match data.get("header") {
        None => {}
        Some(Value::Object(header)) => {
            for key in HEADER_KEYS {
                if let Some(v) = header.get(key)
                    && !v.is_boolean()
                {
                    errors.push(format!(
                        "header.{key}: true 또는 false여야 합니다 (현재: {})",
                        show(v)
                    ));
                }
            }
        }
        Some(_) => errors.push("header: {\"file_name\": true, ...} 형태여야 합니다".to_string()),
    }
    errors
}

/// 검증을 통과한 값(빠진 키는 기본값)으로 Settings를 만든다.
fn settings_from(data: &Map<String, Value>) -> Settings {
    let d = defaults_map();
    let get = |k: &str| data.get(k).unwrap_or(&d[k]);
    let int = |k: &str| as_int(get(k)).unwrap_or_else(|| as_int(&d[k]).unwrap_or(0));
    let text = |k: &str| get(k).as_str().unwrap_or_default().to_string();
    let flag = |k: &str| get(k).as_bool().unwrap_or(false);
    let number = |k: &str| get(k).as_f64().unwrap_or(0.0);

    let mut header = HeaderSettings::default();
    if let Some(Value::Object(h)) = data.get("header") {
        for (k, v) in h {
            if let Some(on) = v.as_bool() {
                header.set(k, on);
            }
        }
    }
    Settings {
        config_version: int("config_version"),
        columns: int("columns"),
        rows: int("rows"),
        width: int("width"),
        gap: int("gap"),
        theme: if text("theme") == "dark" {
            ThemeName::Dark
        } else {
            ThemeName::Light
        },
        language: if text("language") == "ko" {
            Language::Ko
        } else {
            Language::En
        },
        header,
        format: match text("format").as_str() {
            "jpg" => OutputFormat::Jpg,
            "png" => OutputFormat::Png,
            _ => OutputFormat::Webp,
        },
        quality: int("quality"),
        suffix: text("suffix"),
        output_dir: text("output_dir"),
        overwrite: flag("overwrite"),
        show_timestamp: flag("show_timestamp"),
        timestamp_position: TimestampPosition::from_name(&text("timestamp_position")),
        recursive: flag("recursive"),
        skip_start_seconds: number("skip_start_seconds"),
        skip_end_seconds: number("skip_end_seconds"),
        seek_mode: if text("seek_mode") == "accurate" {
            SeekMode::Accurate
        } else {
            SeekMode::Fast
        },
        font: text("font"),
        close_delay_seconds: int("close_delay_seconds"),
    }
}

// --- 파일 위치 ---------------------------------------------------------------

/// `--config`가 있으면 그 파일, 없으면 항상 `%APPDATA%\VidGlance\config.json`.
pub fn resolve_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join("AppData").join("Roaming")
        });
    base.join("VidGlance").join("config.json")
}

// --- 읽기·쓰기 ---------------------------------------------------------------

fn write_json(path: &Path, data: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(data).map_err(std::io::Error::other)? + "\n";
    std::fs::write(path, text)
}

fn json_error(path: &Path, e: &serde_json::Error) -> ConfigError {
    // serde_json 메시지 끝의 " at line X column Y"는 따로 보여 준다
    let full = e.to_string();
    let msg = full.split(" at line ").next().unwrap_or(&full);
    let mut message = format!(
        "{}\n  {}번째 줄 {}번째 글자: JSON 문법 오류 ({msg})",
        path.display(),
        e.line(),
        e.column()
    );
    if msg.contains("escape") {
        message += "\n  Windows 경로의 \\는 \\\\로 두 번 쓰거나 /를 쓰세요. 예: \"D:\\\\영상\" 또는 \"D:/영상\"";
    }
    ConfigError(message)
}

/// 설정 파일을 읽는다. 없으면 기본값으로 만들고, 빠진 키는 채워 다시 저장한다.
///
/// 문법·값 오류면 ConfigError (파일은 건드리지 않는다).
pub fn load_config(path: &Path) -> Result<LoadedConfig, ConfigError> {
    if !path.is_file() {
        write_json(path, &defaults_map()).map_err(|e| {
            ConfigError(format!(
                "설정 파일을 만들 수 없습니다: {}\n  {e}",
                path.display()
            ))
        })?;
        return Ok(LoadedConfig {
            settings: Settings::default(),
            path: path.to_path_buf(),
            created: true,
            warnings: vec![],
        });
    }

    let text = std::fs::read_to_string(path).map_err(|e| {
        ConfigError(format!(
            "설정 파일을 읽을 수 없습니다: {}\n  {e}",
            path.display()
        ))
    })?;
    // 메모장이 붙이는 BOM도 허용한다
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let data: Value = serde_json::from_str(text).map_err(|e| json_error(path, &e))?;
    let Value::Object(data) = data else {
        return Err(ConfigError(format!(
            "{}\n  설정 파일은 {{ ... }} 형태여야 합니다",
            path.display()
        )));
    };

    let errors = validate(&data);
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(ConfigError(format!(
            "{}\n{}",
            path.display(),
            lines.join("\n")
        )));
    }

    let mut warnings: Vec<String> = data
        .keys()
        .filter(|k| k.as_str() != "header" && !is_known(k) && !IGNORED_KEYS.contains(&k.as_str()))
        .map(|k| format!("모르는 설정 키(무시함): {k}"))
        .collect();
    let empty = Map::new();
    let header = match data.get("header") {
        Some(Value::Object(h)) => h,
        _ => &empty,
    };
    warnings.extend(
        header
            .keys()
            .filter(|k| !HEADER_KEYS.contains(&k.as_str()))
            .map(|k| format!("모르는 설정 키(무시함): header.{k}")),
    );

    // 빠진 키를 기본값으로 채워 저장한다. 기존 값·모르는 키는 그대로 둔다.
    let defaults = defaults_map();
    let mut missing: Vec<String> = defaults
        .keys()
        .filter(|k| !data.contains_key(*k))
        .cloned()
        .collect();
    missing.extend(
        HEADER_KEYS
            .iter()
            .filter(|k| !header.contains_key(**k))
            .map(|k| format!("header.{k}")),
    );
    if !missing.is_empty() {
        let mut merged = Map::new();
        for (k, v) in &defaults {
            merged.insert(k.clone(), data.get(k).unwrap_or(v).clone());
        }
        let mut merged_header = defaults["header"].as_object().cloned().unwrap_or_default();
        for (k, v) in header {
            merged_header.insert(k.clone(), v.clone());
        }
        merged.insert("header".into(), Value::Object(merged_header));
        for (k, v) in &data {
            if !merged.contains_key(k) {
                merged.insert(k.clone(), v.clone());
            }
        }
        if let Err(e) = write_json(path, &merged) {
            warnings.push(format!(
                "빠진 설정({})을 파일에 추가하지 못했습니다: {e}",
                missing.join(", ")
            ));
        }
    }

    Ok(LoadedConfig {
        settings: settings_from(&data),
        path: path.to_path_buf(),
        created: false,
        warnings,
    })
}

/// 명령줄 옵션 값으로 덮어쓴다. 값은 설정 파일과 같은 규칙으로 검증한다.
pub fn apply_overrides(
    settings: &Settings,
    overrides: &Map<String, Value>,
) -> Result<Settings, ConfigError> {
    let errors = validate(overrides);
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(ConfigError(format!(
            "명령줄 옵션 오류:\n{}",
            lines.join("\n")
        )));
    }
    let mut data = to_map(settings);
    for (k, v) in overrides {
        data.insert(k.clone(), v.clone());
    }
    Ok(settings_from(&data))
}

fn to_map(s: &Settings) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "config_version": s.config_version,
        "columns": s.columns,
        "rows": s.rows,
        "width": s.width,
        "gap": s.gap,
        "theme": if s.theme == ThemeName::Dark { "dark" } else { "light" },
        "language": if s.language == Language::Ko { "ko" } else { "en" },
        "header": {
            "file_name": s.header.file_name,
            "size": s.header.size,
            "resolution": s.header.resolution,
            "fps": s.header.fps,
            "video_codec": s.header.video_codec,
            "audio_codec": s.header.audio_codec,
            "duration": s.header.duration
        },
        "format": s.format.extension(),
        "quality": s.quality,
        "suffix": s.suffix,
        "output_dir": s.output_dir,
        "overwrite": s.overwrite,
        "show_timestamp": s.show_timestamp,
        "timestamp_position": s.timestamp_position.name(),
        "recursive": s.recursive,
        "skip_start_seconds": s.skip_start_seconds,
        "skip_end_seconds": s.skip_end_seconds,
        "seek_mode": if s.seek_mode == SeekMode::Accurate { "accurate" } else { "fast" },
        "font": s.font,
        "close_delay_seconds": s.close_delay_seconds
    }) else {
        unreachable!()
    };
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("config.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn read(path: &Path) -> Map<String, Value> {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn creates_default_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("한글 폴더").join("config.json");
        let loaded = load_config(&path).unwrap();
        assert!(loaded.created);
        assert_eq!(loaded.settings, Settings::default());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n  \"config_version\": 1,\n  \"columns\": 4,"));
        assert_eq!(read(&path), defaults_map());
    }

    #[test]
    fn default_key_order_python_minus_ffmpeg_dir_plus_timestamp_position() {
        let defaults = defaults_map();
        let keys: Vec<&str> = defaults.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "config_version",
                "columns",
                "rows",
                "width",
                "gap",
                "theme",
                "language",
                "header",
                "format",
                "quality",
                "suffix",
                "output_dir",
                "overwrite",
                "show_timestamp",
                "timestamp_position",
                "recursive",
                "skip_start_seconds",
                "skip_end_seconds",
                "seek_mode",
                "font",
                "close_delay_seconds",
            ]
        );
    }

    #[test]
    fn missing_keys_filled_and_unknown_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            r#"{"columns": 5, "header": {"size": false}, "my_note": "메모"}"#,
        );
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.settings.columns, 5);
        assert_eq!(
            loaded.settings.header,
            HeaderSettings {
                size: false,
                ..HeaderSettings::default()
            }
        );
        let saved = read(&path);
        assert_eq!(saved["columns"], 5);
        assert_eq!(saved["rows"], 5);
        assert_eq!(saved["header"]["size"], false);
        assert_eq!(saved["header"]["duration"], true);
        assert_eq!(saved["my_note"], "메모");
        assert_eq!(loaded.warnings, ["모르는 설정 키(무시함): my_note"]);
    }

    #[test]
    fn python_config_with_ffmpeg_dir_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = defaults_map();
        data.insert("ffmpeg_dir".into(), json!(""));
        let path = write(dir.path(), &serde_json::to_string(&data).unwrap());
        assert_eq!(load_config(&path).unwrap().warnings, Vec::<String>::new());
    }

    #[test]
    fn complete_file_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let text = serde_json::to_string(&defaults_map()).unwrap();
        let path = write(dir.path(), &text);
        load_config(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn bom_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "\u{feff}{\"columns\": 3}");
        assert_eq!(load_config(&path).unwrap().settings.columns, 3);
    }

    #[test]
    fn invalid_values() {
        let cases = [
            (
                r#"{"columns": 0}"#,
                "columns: 1~20 사이 정수여야 합니다 (현재: 0)",
            ),
            (r#"{"rows": 21}"#, "rows: 1~20 사이 정수여야 합니다"),
            (r#"{"columns": 4.0}"#, "columns: 1~20 사이 정수여야 합니다"),
            (r#"{"columns": true}"#, "columns: 1~20 사이 정수여야 합니다"),
            (
                r#"{"width": "1024"}"#,
                r#"width: 320~8000 사이 정수여야 합니다 (현재: "1024")"#,
            ),
            (
                r#"{"theme": "blue"}"#,
                r#"theme: "light" / "dark" 중 하나여야 합니다"#,
            ),
            (
                r#"{"suffix": ""}"#,
                "suffix: 비어 있지 않은 문자열이어야 합니다",
            ),
            (
                r#"{"suffix": "_a/b?"}"#,
                "suffix: 파일명에 쓸 수 없는 문자가 있습니다: / ?",
            ),
            (
                r#"{"overwrite": 1}"#,
                "overwrite: true 또는 false여야 합니다",
            ),
            (
                r#"{"skip_start_seconds": -1}"#,
                "skip_start_seconds: 0 이상의 숫자여야 합니다",
            ),
            (
                r#"{"close_delay_seconds": 1.5}"#,
                "close_delay_seconds: 정수여야 합니다",
            ),
            (
                r#"{"seek_mode": "slow"}"#,
                r#"seek_mode: "fast" / "accurate" 중 하나여야 합니다"#,
            ),
            (
                r#"{"timestamp_position": "middle"}"#,
                r#"timestamp_position: "bottom-right" / "bottom-left" / "top-right" / "top-left" / "center" 중 하나여야 합니다"#,
            ),
            (
                r#"{"header": {"fps": "yes"}}"#,
                "header.fps: true 또는 false여야 합니다",
            ),
            (r#"{"header": []}"#, "header: {"),
        ];
        for (text, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = write(dir.path(), text);
            let err = load_config(&path).unwrap_err();
            assert!(err.0.contains(expected), "{text}: {}", err.0);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn json_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "{\n  \"columns\": 4,\n  \"rows\": 5,\n}");
        assert!(load_config(&path).unwrap_err().0.contains("4번째 줄"));
        let path = write(dir.path(), r#"{"font": "C:\Fonts\a.ttf"}"#);
        assert!(load_config(&path).unwrap_err().0.contains("\\\\로 두 번"));
        let path = write(dir.path(), "[1, 2]");
        assert!(load_config(&path).unwrap_err().0.contains("형태여야"));
    }

    #[test]
    fn overrides() {
        let s = apply_overrides(
            &Settings::default(),
            json!({"columns": 5, "rows": 6, "format": "png"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!((s.columns, s.rows, s.format), (5, 6, OutputFormat::Png));
        assert_eq!(s.width, 1024);
        let err = apply_overrides(
            &Settings::default(),
            json!({"columns": 50}).as_object().unwrap(),
        );
        assert!(err.unwrap_err().0.contains("명령줄 옵션 오류"));
    }

    #[test]
    fn default_location_is_appdata() {
        let path = resolve_config_path(None);
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap();
        assert_eq!(path, appdata.join("VidGlance").join("config.json"));
    }
}
