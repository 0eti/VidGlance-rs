//! 입력 수집(파일/폴더), 영상 한 개 처리, 결과 (spec §5, §9).
//!
//! 화면 출력은 하지 않는다. 진행 표시·요약은 app.rs가 결과 값을 보고 한다.

use crate::config::{OutputFormat, Settings};
use crate::extract::{Source, extract_frames_from, plan_times};
use crate::fonts::FontFiles;
use crate::formatting::header_lines;
use crate::layout::{LayoutInput, WEBP_MAX_SIZE, compute_layout};
use crate::probe;
use crate::render::{render_sheet, save_sheet};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Instant;

pub const VIDEO_EXTENSIONS: [&str; 13] = [
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m2ts", "mpg", "mpeg", "3gp",
]; // fmt: 한 줄에 7개씩
const SHEET_FORMATS: [&str; 3] = ["webp", "jpg", "png"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Done,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputItem {
    pub path: PathBuf,
    pub error: Option<String>, // "파일 없음" 등 처리 전에 이미 실패
}

pub struct Context<'a> {
    pub settings: &'a Settings,
    pub fonts: &'a FontFiles,
    /// 영상 한 개의 칸을 나눠 뽑을 최대 스레드 수
    pub cell_workers: usize,
}

#[derive(Debug)]
pub struct VideoResult {
    pub status: Status,
    pub message: String, // 실패·건너뜀 이유
    pub output: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub seconds: f64,
    pub timings: Vec<(&'static str, f64)>, // 단계별 소요 시간
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Text(String),
    Num(String), // 앞의 0을 뗀 숫자 문자열 (길이 → 사전 순으로 크기 비교)
}

/// 자연 정렬 키: `2.mp4`가 `10.mp4`보다 앞.
fn natural_key(path: &Path) -> Vec<Part> {
    let text = path.to_string_lossy().to_lowercase();
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_digits = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() != in_digits && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        in_digits = ch.is_ascii_digit();
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
        .into_iter()
        .map(|p| {
            if p.starts_with(|c: char| c.is_ascii_digit()) {
                let trimmed = p.trim_start_matches('0');
                Part::Num(if trimmed.is_empty() {
                    "0".into()
                } else {
                    trimmed.into()
                })
            } else {
                Part::Text(p)
            }
        })
        .collect()
}

fn compare_natural(a: &[Part], b: &[Part]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = match (x, y) {
            (Part::Num(p), Part::Num(q)) => p.len().cmp(&q.len()).then_with(|| p.cmp(q)),
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

fn is_sheet(path: &Path, suffix: &str) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    SHEET_FORMATS
        .iter()
        .any(|fmt| name.ends_with(&format!("{suffix}.{fmt}").to_lowercase()))
}

pub fn is_video_ext(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                walk(&path, recursive, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// 파일·폴더가 섞인 인자 → 처리할 영상 목록 (중복 제거, 자연 정렬).
///
/// 폴더는 영상 확장자만 모은다. 직접 지정한 파일은 확장자와 관계없이 넣는다.
/// 이미 만든 시트(`*{suffix}.webp` 등)는 뺀다.
pub fn collect_inputs(args: &[PathBuf], recursive: bool, suffix: &str) -> Vec<InputItem> {
    let mut found = Vec::new();
    for arg in args {
        let path = std::path::absolute(arg).unwrap_or_else(|_| arg.clone());
        if path.is_dir() {
            let mut files = Vec::new();
            walk(&path, recursive, &mut files);
            found.extend(
                files
                    .into_iter()
                    .filter(|p| is_video_ext(p) && !is_sheet(p, suffix))
                    .map(|p| InputItem {
                        path: p,
                        error: None,
                    }),
            );
        } else if path.is_file() {
            if !is_sheet(&path, suffix) {
                found.push(InputItem { path, error: None });
            }
        } else {
            found.push(InputItem {
                path,
                error: Some("파일 없음".into()),
            });
        }
    }

    let mut seen = HashSet::new();
    let mut unique: Vec<InputItem> = found
        .into_iter()
        .filter(|item| {
            let key = std::fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone());
            seen.insert(key.to_string_lossy().to_lowercase())
        })
        .collect();
    unique.sort_by(|a, b| compare_natural(&natural_key(&a.path), &natural_key(&b.path)));
    unique
}

/// `{이름}{suffix}.{format}`. output_dir이 비면 영상 폴더, 상대 경로면 영상 폴더 기준.
pub fn output_path(video: &Path, settings: &Settings) -> PathBuf {
    let mut folder = video.parent().map(Path::to_path_buf).unwrap_or_default();
    if !settings.output_dir.is_empty() {
        folder = folder.join(&settings.output_dir); // 절대 경로면 그대로
    }
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    folder.join(format!(
        "{stem}{}.{}",
        settings.suffix,
        settings.format.extension()
    ))
}

/// 영상 한 개의 시트를 만든다. 에러 대신 VideoResult로 결과를 돌려준다.
pub fn process_video(video: &Path, ctx: &Context) -> VideoResult {
    let began = Instant::now();
    let s = ctx.settings;
    let out = output_path(video, s);
    let mut timings = Vec::new();
    let mut warnings = Vec::new();

    let finish = |status: Status,
                  message: String,
                  output: Option<PathBuf>,
                  warnings: Vec<String>,
                  timings| VideoResult {
        status,
        message,
        output,
        warnings,
        seconds: began.elapsed().as_secs_f64(),
        timings,
    };

    if out.exists() && !s.overwrite {
        return finish(
            Status::Skipped,
            "이미 있음".into(),
            Some(out),
            warnings,
            timings,
        );
    }

    let step = Instant::now();
    let (info, file) = match probe::open(video) {
        Ok(opened) => opened,
        Err(e) if !is_video_ext(video) => {
            return finish(
                Status::Skipped,
                format!("영상 아님 ({e})"),
                None,
                warnings,
                timings,
            );
        }
        Err(e) => return finish(Status::Failed, e.0, None, warnings, timings),
    };
    let open_seconds = step.elapsed().as_secs_f64();
    timings.push(("probe", open_seconds));

    let lines = header_lines(&info, &s.header, s.language);
    let input = LayoutInput {
        width: s.width,
        columns: s.columns,
        rows: s.rows,
        gap: s.gap,
        aspect: info.aspect(),
        header_lines: lines.len() as i64,
    };
    let max_height = (s.format == OutputFormat::Webp).then_some(WEBP_MAX_SIZE);
    let layout = match compute_layout(&input, max_height) {
        Ok(l) => l,
        Err(e) => return finish(Status::Failed, e.0, None, warnings, timings),
    };
    if let Some(original) = layout.shrunk_from {
        warnings.push(format!(
            "WebP 최대 높이({WEBP_MAX_SIZE}px)를 넘어 너비를 {original}px → {}px로 줄였습니다",
            layout.width
        ));
    }

    let step = Instant::now();
    let plan = plan_times(
        info.duration,
        layout.cells.len(),
        s.skip_start_seconds,
        s.skip_end_seconds,
    );
    let size = (layout.cell_width as u32, layout.cell_height as u32);
    let source = Source {
        input: file,
        path: video.to_path_buf(),
        open_seconds,
    };
    let extracted = extract_frames_from(source, &plan.times, size, s.seek_mode, ctx.cell_workers);
    timings.push(("extract", step.elapsed().as_secs_f64()));
    warnings.extend(plan.warnings);
    warnings.extend(extracted.warnings);
    if extracted.frames.iter().all(|f| f.image.is_none()) {
        return finish(
            Status::Failed,
            "프레임을 하나도 뽑지 못했습니다".into(),
            None,
            warnings,
            timings,
        );
    }

    let step = Instant::now();
    let sheet = render_sheet(
        &layout,
        &lines,
        &extracted.frames,
        ctx.fonts,
        s.theme,
        s.show_timestamp.then_some(s.timestamp_position),
    );
    timings.push(("render", step.elapsed().as_secs_f64()));

    let step = Instant::now();
    let saved = out
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| save_sheet(&sheet, &out, s.format, s.quality));
    if let Err(e) = saved {
        return finish(
            Status::Failed,
            format!("저장할 수 없음: {e}"),
            None,
            warnings,
            timings,
        );
    }
    timings.push(("save", step.elapsed().as_secs_f64()));

    finish(Status::Done, String::new(), Some(out), warnings, timings)
}

/// 항목 하나를 처리한다. 처리 전에 이미 실패한 항목(파일 없음 등)은 바로 실패 결과.
pub fn process_item(item: &InputItem, ctx: &Context) -> VideoResult {
    match &item.error {
        Some(error) => VideoResult {
            status: Status::Failed,
            message: error.clone(),
            output: None,
            warnings: vec![],
            seconds: 0.0,
            timings: vec![],
        },
        None => process_video(&item.path, ctx),
    }
}

/// 영상들을 jobs개 스레드로 동시에 처리하고, 결과는 입력 순서대로 report에 넘긴다.
///
/// before(i)는 i번째 결과를 기다리기 전에 불린다 (진행 표시용). stop()이 true가 되면 새 영상은
/// 시작하지 않고 이미 시작한 영상까지만 넘긴다. 결과를 넘긴 개수를 돌려준다.
pub fn process_in_order(
    items: &[InputItem],
    ctx: &Context,
    jobs: usize,
    stop: &(dyn Fn() -> bool + Sync),
    mut before: impl FnMut(usize),
    mut report: impl FnMut(usize, VideoResult),
) -> usize {
    // (시작한 영상 수, 더 시작하지 않음)
    let claim = Mutex::new((0usize, false));
    let lock = || claim.lock().unwrap_or_else(|e| e.into_inner());
    let take = || {
        let mut c = lock();
        if c.1 || c.0 >= items.len() || stop() {
            c.1 = true;
            return None;
        }
        c.0 += 1;
        Some(c.0 - 1)
    };
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, items.len().max(1)) {
            let tx = tx.clone();
            scope.spawn(move || {
                while let Some(i) = take() {
                    if tx.send((i, process_item(&items[i], ctx))).is_err() {
                        return;
                    }
                }
            });
        }
        drop(tx);

        let mut finished = HashMap::new();
        for i in 0..items.len() {
            {
                let mut c = lock();
                if i >= c.0 && (c.1 || stop()) {
                    c.1 = true;
                    return i;
                }
            }
            before(i);
            let result = loop {
                if let Some(r) = finished.remove(&i) {
                    break Some(r);
                }
                match rx.recv() {
                    Ok((j, r)) => {
                        finished.insert(j, r);
                    }
                    Err(_) => break None, // 모든 스레드가 끝남: i번째는 시작되지 않았다
                }
            };
            match result {
                Some(r) => report(i, r),
                None => return i,
            }
        }
        items.len()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::find_fonts;
    use crate::test_media::media;

    fn names(items: &[InputItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("영상 폴더");
        std::fs::create_dir_all(root.join("하위")).unwrap();
        for name in [
            "10화.mp4",
            "2화.mp4",
            "UPPER.MKV",
            "2화_sheet.webp",
            "메모.txt",
        ] {
            std::fs::write(root.join(name), b"").unwrap();
        }
        std::fs::write(root.join("하위").join("3화.mp4"), b"").unwrap();
        dir
    }

    #[test]
    fn collect_filters_sorts_dedups() {
        let dir = folder();
        let root = dir.path().join("영상 폴더");
        let items = collect_inputs(std::slice::from_ref(&root), false, "_sheet");
        assert_eq!(names(&items), ["2화.mp4", "10화.mp4", "UPPER.MKV"]);
        assert!(
            names(&collect_inputs(std::slice::from_ref(&root), true, "_sheet"))
                .contains(&"3화.mp4".to_string())
        );

        let args = [
            root.join("2화.mp4"),
            root.clone(),
            root.join("메모.txt"),
            root.join("2화_sheet.webp"),
            root.join("없는 파일.mp4"),
        ];
        let items = collect_inputs(&args, false, "_sheet");
        assert_eq!(
            names(&items),
            [
                "2화.mp4",
                "10화.mp4",
                "UPPER.MKV",
                "메모.txt",
                "없는 파일.mp4"
            ]
        );
        assert_eq!(items.last().unwrap().error.as_deref(), Some("파일 없음"));
    }

    #[test]
    fn output_paths() {
        let video = PathBuf::from("D:/영상/예시 영상.mp4");
        assert_eq!(
            output_path(&video, &Settings::default()),
            PathBuf::from("D:/영상/예시 영상_sheet.webp")
        );
        let s = Settings {
            suffix: "_thumb".into(),
            format: OutputFormat::Png,
            output_dir: "thumbs".into(),
            ..Settings::default()
        };
        assert_eq!(
            output_path(&PathBuf::from("D:/영상/a.mkv"), &s),
            PathBuf::from("D:/영상/thumbs/a_thumb.png")
        );
    }

    #[test]
    fn process_done_skip_overwrite_fail() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = find_fonts("");
        let video = dir.path().join("한글 영상 (1)&.mp4");
        std::fs::copy(media("무음.mp4"), &video).unwrap();
        let settings = Settings::default();
        let ctx = Context {
            settings: &settings,
            fonts: &fonts,
            cell_workers: 4,
        };

        let r = process_video(&video, &ctx);
        assert_eq!(r.status, Status::Done, "{}", r.message);
        let out = r.output.unwrap();
        assert_eq!(out, dir.path().join("한글 영상 (1)&_sheet.webp"));
        let names: Vec<&str> = r.timings.iter().map(|t| t.0).collect();
        assert_eq!(names, ["probe", "extract", "render", "save"]);

        let r = process_video(&video, &ctx);
        assert_eq!(
            (r.status, r.message.as_str()),
            (Status::Skipped, "이미 있음")
        );

        let broken = dir.path().join("깨짐.mp4");
        std::fs::copy(media("깨짐.mp4"), &broken).unwrap();
        let r = process_video(&broken, &ctx);
        assert_eq!(r.status, Status::Failed);
        assert!(r.message.contains("영상을 읽을 수 없음"));

        let doc = dir.path().join("메모.txt");
        std::fs::write(&doc, "영상 아님").unwrap();
        let r = process_video(&doc, &ctx);
        assert_eq!(r.status, Status::Skipped);
        assert!(r.message.starts_with("영상 아님"));
    }

    #[test]
    fn parallel_results_in_input_order_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = find_fonts("");
        let mut items: Vec<InputItem> = (1..=4)
            .map(|i| {
                let path = dir.path().join(format!("{i}화.mp4"));
                std::fs::copy(media("무음.mp4"), &path).unwrap();
                InputItem { path, error: None }
            })
            .collect();
        items.insert(
            2,
            InputItem {
                path: dir.path().join("없음.mp4"),
                error: Some("파일 없음".into()),
            },
        );
        let settings = Settings::default();
        let ctx = Context {
            settings: &settings,
            fonts: &fonts,
            cell_workers: 1,
        };

        let mut before = Vec::new();
        let mut reported = Vec::new();
        let n = process_in_order(
            &items,
            &ctx,
            3,
            &|| false,
            |i| before.push(i),
            |i, r| reported.push((i, r.status)),
        );
        assert_eq!(n, 5);
        assert_eq!(before, [0, 1, 2, 3, 4]);
        use Status::*;
        assert_eq!(
            reported,
            [(0, Done), (1, Done), (2, Failed), (3, Done), (4, Done)]
        );
        for i in [1, 2, 3, 4] {
            assert!(dir.path().join(format!("{i}화_sheet.webp")).exists());
        }

        // 처음부터 멈춤: 아무것도 시작하지 않는다
        let mut calls = 0;
        let n = process_in_order(&items, &ctx, 3, &|| true, |_| calls += 1, |_, _| {});
        assert_eq!((n, calls), (0, 0));
    }
}
