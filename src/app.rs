//! 진입점: 인자 파싱, 콘솔 출력, 창 대기, 종료 코드 (spec §2, §10, §11).
//!
//! 출력·입력 대기·종료 코드는 이 모듈에서만 다룬다.

use crate::batch::{
    Context, InputItem, Status, VideoResult, collect_inputs, output_paths, pending_count,
    process_in_order,
};
use crate::config::{
    ConfigError, LoadedConfig, Settings, apply_overrides, load_config, resolve_config_path,
};
use crate::fonts::{FontFiles, find_fonts};
use crate::probe;
use crate::render::SAVING;
use clap::Parser;
use serde_json::{Map, Value, json};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_SETUP: i32 = 2; // 설정 오류·잘못된 인자

const RULE: &str = "──────────";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse_grid(text: &str) -> Result<(i64, i64), String> {
    let lower = text.to_lowercase();
    let mut parts = lower.split('x');
    match (
        parts.next().map(str::parse),
        parts.next().map(str::parse),
        parts.next(),
    ) {
        (Some(Ok(c)), Some(Ok(r)), None) => Ok((c, r)),
        _ => Err(format!("격자는 4x5 형식이어야 합니다: {text}")),
    }
}

/// spec §10. 지정한 옵션은 config.json 값을 덮어쓴다.
#[derive(Parser, Debug)]
#[command(
    name = "VidGlance",
    version,
    about = "영상 파일·폴더마다 정보 헤더 + 프레임 격자 시트를 만듭니다."
)]
pub struct Args {
    /// 영상 파일 또는 폴더
    pub inputs: Vec<PathBuf>,
    /// 격자 (예: 4x5)
    #[arg(short = 'g', long, value_name = "CxR", value_parser = parse_grid)]
    pub grid: Option<(i64, i64)>,
    #[arg(short = 'w', long, value_name = "PX")]
    pub width: Option<i64>,
    #[arg(long, value_name = "PX")]
    pub gap: Option<i64>,
    #[arg(long, value_parser = ["light", "dark"])]
    pub theme: Option<String>,
    #[arg(long, value_parser = ["en", "ko"])]
    pub lang: Option<String>,
    #[arg(short = 'o', long, value_name = "DIR")]
    pub output_dir: Option<String>,
    #[arg(short = 'r', long)]
    pub recursive: bool,
    #[arg(long, value_parser = ["webp", "jpg", "png"])]
    pub format: Option<String>,
    #[arg(long, value_name = "N")]
    pub quality: Option<i64>,
    #[arg(long)]
    pub overwrite: bool,
    #[arg(long)]
    pub no_timestamp: bool,
    #[arg(long, value_name = "PATH")]
    pub font: Option<String>,
    /// 프레임 고르는 방식
    #[arg(long, value_parser = ["fast", "accurate"])]
    pub seek: Option<String>,
    /// 설정 파일 직접 지정 (기본: %APPDATA%\VidGlance\config.json)
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// 끝나고 기다리지 않음
    #[arg(long)]
    pub no_pause: bool,
    /// 요약만 출력
    #[arg(short, long)]
    pub quiet: bool,
    /// FFmpeg 버전·폰트·단계별 시간 출력
    #[arg(short, long)]
    pub verbose: bool,
}

/// 명령줄에서 지정한 값만 설정 키 이름으로.
fn overrides(args: &Args) -> Map<String, Value> {
    let mut m = Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            m.insert(k.into(), v);
        }
    };
    put("width", args.width.map(|v| json!(v)));
    put("gap", args.gap.map(|v| json!(v)));
    put("theme", args.theme.clone().map(Value::from));
    put("language", args.lang.clone().map(Value::from));
    put("output_dir", args.output_dir.clone().map(Value::from));
    put("recursive", args.recursive.then_some(json!(true)));
    put("format", args.format.clone().map(Value::from));
    put("quality", args.quality.map(|v| json!(v)));
    put("overwrite", args.overwrite.then_some(json!(true)));
    put("font", args.font.clone().map(Value::from));
    put("seek_mode", args.seek.clone().map(Value::from));
    put("show_timestamp", args.no_timestamp.then_some(json!(false)));
    if let Some((c, r)) = args.grid {
        put("columns", Some(json!(c)));
        put("rows", Some(json!(r)));
    }
    m
}

// --- 콘솔 창 ----------------------------------------------------------------

unsafe extern "C" {
    fn _getwch() -> u16;
    fn _kbhit() -> i32;
}

/// 이 프로그램을 위해 새로 열린 콘솔 창인지 (드래그·더블클릭). spec §2.4.
/// Rust exe는 부트로더·런처가 없으므로 콘솔에 붙은 프로세스가 자기 하나뿐이면 된다.
pub fn owns_console() -> bool {
    let mut pids = [0u32; 4];
    let count =
        unsafe { windows_sys::Win32::System::Console::GetConsoleProcessList(pids.as_mut_ptr(), 4) };
    count == 1
}

fn wait_key() {
    println!("아무 키나 누르면 닫힙니다…");
    let _ = std::io::stdout().flush();
    unsafe {
        _getwch();
    }
}

fn countdown(seconds: i64) {
    for left in (1..=seconds).rev() {
        print!("\r{left}초 뒤 닫힙니다… (아무 키나 누르면 바로 닫힘)");
        let _ = std::io::stdout().flush();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if unsafe { _kbhit() } != 0 {
                unsafe {
                    _getwch();
                }
                println!();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    println!();
}

/// spec §2.4: 실패가 있거나 delay < 0이면 키 대기, 아니면 카운트다운.
fn finish(pause: bool, ok: bool, delay: i64) {
    if !pause {
        return;
    }
    if !ok || delay < 0 {
        wait_key();
    } else if delay > 0 {
        countdown(delay);
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Ctrl+C·Ctrl+Break는 지금 처리 중인 영상까지만 하고 멈춘다. 창 닫기는 저장 중인 임시 파일을 지운다.
unsafe extern "system" fn on_console_event(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    INTERRUPTED.store(true, Ordering::SeqCst);
    if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
        // 창 닫기·로그오프·종료: 이 함수가 끝나면 프로세스가 끝난다
        let saving = SAVING.lock().map(|g| g.clone()).unwrap_or_default();
        for tmp in saving {
            let _ = std::fs::remove_file(tmp);
        }
    }
    1
}

/// 동시에 처리할 영상 수. 4K AV1 5개에서 1개씩 1.6초 → 3개씩 1.0초, 더 늘려도 비슷했다.
const VIDEO_JOBS: usize = 3;
/// 영상이 하나일 때 칸을 나눠 뽑을 스레드 수 (4K AV1 0.34초 → 0.30초).
/// 영상을 동시에 처리할 때는 코어가 이미 차서 나누지 않는다.
const CELL_WORKERS: usize = 4;

/// (동시에 처리할 영상 수, 영상 하나의 칸을 나눠 뽑을 스레드 수)
fn parallelism(videos: usize) -> (usize, usize) {
    let jobs = VIDEO_JOBS.min(videos).max(1);
    (jobs, if jobs == 1 { CELL_WORKERS } else { 1 })
}

// --- 출력 -------------------------------------------------------------------

fn describe(r: &VideoResult) -> String {
    let seconds = format!("{:.1}s", r.seconds);
    match (r.status, &r.output) {
        (Status::Done, Some(out)) => {
            format!(
                "완료 ({seconds}) → {}",
                out.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            )
        }
        (Status::Skipped, _) => format!("건너뜀 ({})", r.message),
        _ => format!("실패: {}", r.message),
    }
}

fn stage_name(key: &str) -> &str {
    match key {
        "extract" => "추출",
        "render" => "렌더",
        "save" => "저장",
        other => other,
    }
}

fn timings(r: &VideoResult) -> String {
    r.timings
        .iter()
        .map(|(k, v)| format!("{} {v:.2}s", stage_name(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_setup(loaded: &LoadedConfig, s: &Settings, verbose: bool, fonts: &FontFiles) {
    let created = if loaded.created {
        " (새로 만듦)"
    } else {
        ""
    };
    println!(
        "VidGlance {VERSION}  |  설정: {}{created}  |  {}x{}, {}px, {}",
        loaded.path.display(),
        s.columns,
        s.rows,
        s.width,
        s.format.extension()
    );
    for w in loaded.warnings.iter().chain(&fonts.warnings) {
        println!("경고: {w}");
    }
    if verbose {
        println!("{}", probe::backend_version());
        println!("폰트: {}", fonts.source);
    }
}

// --- 진입점 -----------------------------------------------------------------

fn run(args: &Args, pause: bool) -> i32 {
    let began = Instant::now();
    let loaded_and_settings =
        load_config(&resolve_config_path(args.config.as_deref())).and_then(|l| {
            let settings = apply_overrides(&l.settings, &overrides(args))?;
            Ok((l, settings))
        });
    let (loaded, settings) = match loaded_and_settings {
        Ok(pair) => pair,
        Err(ConfigError(e)) => {
            println!("설정 오류: {e}\n\n설정 파일을 고친 뒤 다시 실행하세요.");
            finish(pause, false, 0);
            return EXIT_SETUP;
        }
    };

    if args.inputs.is_empty() {
        let created = if loaded.created {
            " (새로 만들었습니다)"
        } else {
            ""
        };
        println!("VidGlance {VERSION}");
        println!("영상 파일이나 폴더를 VidGlance.exe 위로 끌어다 놓으세요.");
        println!("여러 개를 한꺼번에 놓을 수 있습니다.");
        println!("설정 파일: {}{created}", loaded.path.display());
        finish(pause, false, 0); // 안내를 읽을 수 있게 키 대기
        return EXIT_OK;
    }

    let fonts = find_fonts(&settings.font);
    if !args.quiet {
        print_setup(&loaded, &settings, args.verbose, &fonts);
    }
    let items: Vec<InputItem> = collect_inputs(&args.inputs, settings.recursive, &settings.suffix);
    let outputs = output_paths(&items, &settings);
    let (jobs, cell_workers) = parallelism(pending_count(&items, &outputs, settings.overwrite));
    let ctx = Context {
        settings: &settings,
        fonts: &fonts,
        cell_workers,
    };
    let mut counts: HashMap<Status, usize> = HashMap::new();
    let width = items.len().to_string().len();
    let waiting = Cell::new(false); // "[i/n] 이름 … "을 찍고 결과를 기다리는 중
    let stop = || INTERRUPTED.load(Ordering::SeqCst);
    let reported = process_in_order(
        &items,
        &outputs,
        &ctx,
        jobs,
        &stop,
        |index| {
            if !args.quiet {
                let name = items[index]
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                print!("[{:>width$}/{}] {name} … ", index + 1, items.len());
                let _ = std::io::stdout().flush();
                waiting.set(true);
            }
        },
        |_, result| {
            *counts.entry(result.status).or_default() += 1;
            if !args.quiet {
                waiting.set(false);
                println!("{}", describe(&result));
                for w in &result.warnings {
                    println!("    경고: {w}");
                }
                if args.verbose && !result.timings.is_empty() {
                    println!("    {}", timings(&result));
                }
            }
        },
    );
    if waiting.get() {
        println!(); // 중단되어 시작하지 않은 영상
    }
    let interrupted = reported < items.len() || INTERRUPTED.load(Ordering::SeqCst);

    if !args.quiet {
        println!("{RULE}");
    }
    if items.is_empty() {
        println!("처리할 영상이 없습니다.");
    }
    let count = |s: Status| counts.get(&s).copied().unwrap_or(0);
    println!(
        "완료 {}, 건너뜀 {}, 실패 {} (총 {:.1}s){}",
        count(Status::Done),
        count(Status::Skipped),
        count(Status::Failed),
        began.elapsed().as_secs_f64(),
        if interrupted { " — 중단됨" } else { "" }
    );

    let ok = count(Status::Failed) == 0 && !interrupted;
    finish(pause, ok, settings.close_delay_seconds);
    if ok { EXIT_OK } else { EXIT_FAILED }
}

pub fn main() -> i32 {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_SETUP } else { EXIT_OK }; // --help·--version은 0
        }
    };
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_console_event), 1);
    }
    let pause = !args.no_pause && owns_console();
    run(&args, pause)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_parse() {
        assert_eq!(parse_grid("4x5"), Ok((4, 5)));
        assert_eq!(parse_grid("5X6"), Ok((5, 6)));
        assert!(parse_grid("4by5").is_err());
    }

    #[test]
    fn overrides_only_given() {
        let args = Args::parse_from([
            "VidGlance",
            "-g",
            "2x3",
            "--seek",
            "accurate",
            "--no-timestamp",
            "a.mp4",
        ]);
        let m = overrides(&args);
        assert_eq!(m["columns"], 2);
        assert_eq!(m["rows"], 3);
        assert_eq!(m["seek_mode"], "accurate");
        assert_eq!(m["show_timestamp"], false);
        assert!(!m.contains_key("width"));
    }

    #[test]
    fn terminal_does_not_own_console() {
        assert!(!owns_console());
    }
}
