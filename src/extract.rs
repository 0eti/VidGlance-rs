//! 시점 계산, 프레임 추출 (spec §7). Python 버전의 PyAV 동작을 FFmpeg 라이브러리로 그대로 옮겼다.

use crate::config::SeekMode;
use crate::formatting::format_duration;
use crate::probe;
use ffmpeg::ffi;
use ffmpeg::software::scaling;
use ffmpeg::util::format::Pixel;
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::path::{Path, PathBuf};

/// 여는 데 이보다 오래 걸린 파일(색인 없는 fragmented MP4 등)은 다시 열지 않고 칸을 혼자 뽑는다.
const REOPEN_LIMIT_SECONDS: f64 = 0.1;

/// 칸 사이 최소 간격(초). 이보다 촘촘하면 "매우 짧은 영상"으로 본다.
pub const MIN_SPACING: f64 = 0.1;
/// 프레임 시각 비교 허용 오차(초, 부동소수 오차용)
const EPSILON: f64 = 1e-3;
/// fast 모드에서 요청 시각과 키프레임이 이보다 멀면(초) 정확한 시점으로 뽑는다.
const MAX_KEYFRAME_OFFSET: f64 = 10.0;

/// fast 모드 허용 오차: 칸 간격의 절반과 10초 중 작은 값 (칸이 하나면 정확하게).
fn fast_tolerance(times: &[f64]) -> f64 {
    match times {
        [first, second, ..] => ((second - first) / 2.0).min(MAX_KEYFRAME_OFFSET),
        _ => 0.0,
    }
}

/// 뽑을 시점 목록. 영상이 매우 짧으면 칸 수보다 적다.
#[derive(Debug, Clone, PartialEq)]
pub struct FramePlan {
    pub times: Vec<f64>,
    pub warnings: Vec<String>,
}

/// 칸 하나의 결과. image가 None이면 추출 실패(회색 칸).
/// time은 실제로 뽑은 프레임의 시각(초, 파일 시작 기준). 실패하면 요청 시각.
#[derive(Debug, Clone)]
pub struct Frame {
    pub time: f64,
    pub image: Option<RgbImage>,
}

#[derive(Debug)]
pub struct ExtractResult {
    pub frames: Vec<Frame>,
    pub warnings: Vec<String>,
}

/// `t_i = S + L·i/(N+1)` (i = 1..N). 처음과 끝은 제외한 균등 간격.
pub fn plan_times(duration: f64, count: usize, skip_start: f64, skip_end: f64) -> FramePlan {
    let mut warnings = Vec::new();
    let min_length = count as f64 * MIN_SPACING;

    let (mut start, mut end) = (skip_start, duration - skip_end);
    if (skip_start != 0.0 || skip_end != 0.0) && end - start < min_length {
        warnings.push(format!(
            "앞뒤 제외 구간을 빼면 {:.1}초만 남아 제외 구간을 무시합니다",
            (end - start).max(0.0)
        ));
        (start, end) = (0.0, duration);
    }
    let length = end - start;

    let mut n = count;
    if length < min_length {
        // 간격이 0.1초 이상이 되는 최대 개수 (부동소수 오차 보정 후 내림)
        n = ((length / MIN_SPACING + 1e-9).floor() as i64 - 1).max(1) as usize;
        warnings.push(format!("영상이 짧아 {count}칸 중 {n}칸만 채웁니다"));
    }

    let times = (1..=n)
        .map(|i| start + length * i as f64 / (n + 1) as f64)
        .collect();
    FramePlan { times, warnings }
}

/// probe::open이 연 파일. 칸을 나눠 뽑을 때 다른 스레드는 path를 다시 연다.
pub struct Source {
    pub input: ffmpeg::format::context::Input,
    pub path: PathBuf,
    pub open_seconds: f64,
}

/// 키프레임 조회 결과: (seek 시각, (pts, 초))
type PeekCache = (i64, Option<(i64, f64)>);

/// swscale 설정 키: (원본 픽셀 형식, 원본 가로, 원본 세로, 출력 가로, 출력 세로)
type ScalerKey = (Pixel, u32, u32, u32, u32);

/// 열린 영상 하나: seek·디코딩을 반복한다 (파일은 한 번만 연다).
struct Decoding {
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    time_base: f64,
    parameters: ffmpeg::codec::Parameters,
    threads: usize,
    /// 정확한 시점까지 연달아 디코딩할 때: 프레임·슬라이스 스레드 (처리량 위주).
    /// 처음 필요할 때 연다 (fast 모드에서 키프레임만 쓰면 열지 않는다).
    sequential: Option<ffmpeg::decoder::Video>,
    /// 키프레임 한 장만 뽑을 때: 저지연 (패킷 하나 넣으면 바로 나옴). 프레임 스레드는
    /// 첫 프레임 전에 패킷을 여러 장 받아 디코딩하므로 한 장만 필요할 때 2배 넘게 느리다.
    single: ffmpeg::decoder::Video,
    eof_sent: bool,
    /// sequential 디코더가 마지막으로 내놓은 프레임 시각. seek하면 None.
    /// 다음 칸이 같은 키프레임 구간이면 seek 없이 여기서 이어 디코딩한다.
    position: Option<f64>,
    /// 키프레임 위치만 알아볼 때 쓰는 같은 파일의 두 번째 열기. 본 파일의 디코딩 위치를 지킨다.
    /// reopen이 None이면(여는 데 오래 걸리는 파일) 열지 않고 본 파일에서 알아본다.
    peeker: Option<ffmpeg::format::context::Input>,
    reopen: Option<PathBuf>,
    /// 방향별 마지막 키프레임 조회 ([직후, 직전]). fast 모드가 앞뒤를 둘 다 본 뒤 정확한 시점으로
    /// 넘어가면 직전 조회를 다시 쓴다.
    last_peek: [Option<PeekCache>; 2],
    /// fast 모드에서 키프레임 한 장으로 끝낸 칸 수
    keyframe_hits: usize,
    rotation: i32,
    scaler: Option<(scaling::Context, ScalerKey)>,
}

/// 지금 위치에서 stream의 첫 패킷 시각 (pts, 없으면 dts).
fn first_packet_pts(
    input: &mut ffmpeg::format::context::Input,
    stream_index: usize,
) -> Option<i64> {
    let mut packet = ffmpeg::Packet::empty();
    loop {
        packet.read(input).ok()?;
        if packet.stream() == stream_index
            && let Some(pts) = packet.pts().or_else(|| packet.dts())
        {
            return Some(pts);
        }
    }
}

/// stream 기준 time_base 단위 ts로 seek한다 (backward면 ts 이전 키프레임).
fn seek_input(
    input: &mut ffmpeg::format::context::Input,
    stream_index: usize,
    ts: i64,
    backward: bool,
) -> Result<(), ffmpeg::Error> {
    let flags = if backward {
        ffi::AVSEEK_FLAG_BACKWARD
    } else {
        0
    };
    let ret = unsafe { ffi::av_seek_frame(input.as_mut_ptr(), stream_index as i32, ts, flags) };
    if ret < 0 {
        return Err(ffmpeg::Error::from(ret));
    }
    Ok(())
}

// SwsContext·디코더는 스레드에 묶이지 않으므로 다른 스레드로 넘겨도 된다 (동시에 쓰지는 않음).
unsafe impl Send for Decoding {}

/// 디코더를 연다. threads가 0이면 스레드 개수 자동.
fn open_decoder(
    parameters: &ffmpeg::codec::Parameters,
    threads: usize,
    low_delay: bool,
) -> Result<ffmpeg::decoder::Video, ffmpeg::Error> {
    let mut ctx = ffmpeg::codec::context::Context::from_parameters(parameters.clone())?;
    unsafe {
        let raw = ctx.as_mut_ptr();
        (*raw).thread_count = threads as i32;
        if low_delay {
            (*raw).thread_type = ffi::FF_THREAD_SLICE;
            (*raw).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
        } else {
            // PyAV의 thread_type = "AUTO"와 같게
            (*raw).thread_type = ffi::FF_THREAD_FRAME | ffi::FF_THREAD_SLICE;
        }
    }
    ctx.decoder().video()
}

impl Decoding {
    fn open(path: &Path, threads: usize) -> Result<Self, ffmpeg::Error> {
        probe::init();
        Self::from_input(probe::open_input(path)?, threads, Some(path))
    }

    /// 이미 연 파일로 시작한다 (probe::open이 연 것을 그대로 쓴다).
    /// reopen: 키프레임 조회용으로 다시 열어도 되는 경로.
    fn from_input(
        input: ffmpeg::format::context::Input,
        threads: usize,
        reopen: Option<&Path>,
    ) -> Result<Self, ffmpeg::Error> {
        let stream = probe::first_video(&input).ok_or(ffmpeg::Error::StreamNotFound)?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = tb.numerator() as f64 / tb.denominator() as f64;
        let rotation =
            ((probe::stream_rotation(&stream) as f64 / 90.0).round() as i32 * 90).rem_euclid(360);
        let parameters = stream.parameters();
        let single = open_decoder(&parameters, threads, true)?;
        Ok(Decoding {
            input,
            stream_index,
            time_base,
            parameters,
            threads,
            sequential: None,
            single,
            eof_sent: false,
            position: None,
            peeker: None,
            reopen: reopen.map(Path::to_path_buf),
            last_peek: [None, None],
            keyframe_hits: 0,
            rotation,
            scaler: None,
        })
    }

    /// 파일 시작 시각(초). ffmpeg의 입력 seek처럼 이것을 기준으로 맞춘다.
    fn start_time(&self) -> f64 {
        let st = unsafe { (*self.input.as_ptr()).start_time };
        if st == ffi::AV_NOPTS_VALUE || st == 0 {
            0.0
        } else {
            st as f64 / ffi::AV_TIME_BASE as f64
        }
    }

    /// target(초) 직전(backward) 또는 직후 키프레임으로 seek하고 디코더를 비운다.
    fn seek(&mut self, target: f64, backward: bool) -> Result<(), ffmpeg::Error> {
        // PyAV int(target / time_base)와 같은 버림
        self.seek_ts((target / self.time_base) as i64, backward)
    }

    /// 스트림 time_base 단위 시각으로 seek한다.
    fn seek_ts(&mut self, ts: i64, backward: bool) -> Result<(), ffmpeg::Error> {
        self.position = None;
        seek_input(&mut self.input, self.stream_index, ts, backward)?;
        if let Some(decoder) = self.sequential.as_mut() {
            decoder.flush();
        }
        self.single.flush();
        self.eof_sent = false;
        Ok(())
    }

    /// 다음 디코딩된 프레임과 그 시각(초). 끝이면 None. single이면 저지연 디코더를 쓴다.
    fn next_frame(
        &mut self,
        single: bool,
    ) -> Result<Option<(ffmpeg::frame::Video, f64)>, ffmpeg::Error> {
        if !single && self.sequential.is_none() {
            self.sequential = Some(open_decoder(&self.parameters, self.threads, false)?);
        }
        let mut frame = ffmpeg::frame::Video::empty();
        loop {
            let decoder = match (single, self.sequential.as_mut()) {
                (false, Some(sequential)) => sequential,
                _ => &mut self.single,
            };
            match decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    if let Some(pts) = frame.pts().or_else(|| frame.timestamp()) {
                        let t = pts as f64 * self.time_base;
                        return Ok(Some((frame, t)));
                    }
                    continue; // 시각 없는 프레임은 건너뛴다 (PyAV와 같게)
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno }) if errno == ffi::EAGAIN => {}
                Err(e) => return Err(e),
            }
            if self.eof_sent {
                return Ok(None);
            }
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) => {
                    if packet.stream() == self.stream_index {
                        // 손상된 패킷 하나는 넘어간다 (다음 패킷으로 계속)
                        let _ = decoder.send_packet(&packet);
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    let _ = decoder.send_eof();
                    self.eof_sent = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// target 이전 키프레임부터 디코딩해 target 이상인 첫 프레임 (ffmpeg 입력 seek과 같음).
    ///
    /// 직전 칸에서 디코딩한 위치와 target 사이에 키프레임이 없으면 seek하지 않고 이어서
    /// 디코딩한다. seek해도 같은 키프레임(또는 그 앞)부터 같은 프레임을 다시 디코딩할 뿐이라
    /// 결과는 같고 일은 줄어든다.
    fn accurate(&mut self, target: f64) -> Option<(ffmpeg::frame::Video, f64)> {
        let position = self.position;
        let resume = match position {
            Some(pos) if pos < target - EPSILON && self.can_peek() => self
                .keyframe_pts(target, true)
                .is_some_and(|(_, key)| key <= pos + EPSILON),
            _ => false,
        };
        if !resume {
            self.seek(target, true).ok()?;
        }
        self.position = None;
        while let Ok(Some((frame, t))) = self.next_frame(false) {
            if t >= target - EPSILON {
                self.position = Some(t);
                return Some((frame, t));
            }
        }
        None // target 이후 프레임이 없음 (끝을 넘음)
    }

    /// 키프레임 조회용 두 번째 열기를 (없으면 열어서) 쓸 수 있는지.
    fn can_peek(&mut self) -> bool {
        if self.peeker.is_none()
            && let Some(path) = self.reopen.take()
        {
            self.peeker = probe::open_input(&path).ok();
            if self.peeker.is_some() {
                self.reopen = Some(path);
            }
        }
        self.peeker.is_some()
    }

    /// seek(target, backward)이 닿는 키프레임(target 직전/직후)의 시각을 디코딩 없이
    /// 패킷만 읽어 알아낸다: (pts, 초). 두 번째 열기가 있으면 거기서 읽어 디코딩 위치를 지킨다.
    fn keyframe_pts(&mut self, target: f64, backward: bool) -> Option<(i64, f64)> {
        let ts = (target / self.time_base) as i64; // seek()과 같은 버림
        let slot = backward as usize;
        if let Some((key, found)) = self.last_peek[slot]
            && key == ts
        {
            return found;
        }
        let found = if self.can_peek() {
            let peeker = self.peeker.as_mut()?;
            seek_input(peeker, self.stream_index, ts, backward).ok()?;
            first_packet_pts(peeker, self.stream_index)
        } else {
            self.seek_ts(ts, backward).ok()?;
            first_packet_pts(&mut self.input, self.stream_index)
        }
        .map(|pts| (pts, pts as f64 * self.time_base));
        self.last_peek[slot] = Some((ts, found));
        found
    }

    /// target 앞뒤 키프레임 중 더 가까운 쪽: (pts, 초). 패킷 시각만 보고 고른다.
    fn nearest_keyframe(&mut self, target: f64) -> Option<(i64, f64)> {
        let before = self.keyframe_pts(target, true);
        let after = self.keyframe_pts(target, false);
        match (before, after) {
            (Some(b), Some(a)) => Some(if (b.1 - target).abs() <= (a.1 - target).abs() {
                b
            } else {
                a
            }),
            (b, a) => b.or(a),
        }
    }

    /// seek_mode에 맞게 target(절대 시각)의 프레임을 고른다.
    /// fast: 가장 가까운 키프레임이 tolerance(칸 간격의 절반, 최대 10초) 안이면 그 한 장만 저지연 디코더로
    /// 디코딩하고, 더 멀면 정확하게.
    fn grab(
        &mut self,
        target: f64,
        mode: SeekMode,
        tolerance: f64,
    ) -> Option<(ffmpeg::frame::Video, f64)> {
        if mode == SeekMode::Fast
            && let Some((pts, key)) = self.nearest_keyframe(target)
            && (key - target).abs() <= tolerance
            && self.seek_ts(pts, true).is_ok()
            && let Ok(Some((frame, t))) = self.next_frame(true)
            && (t - target).abs() <= tolerance
        {
            self.keyframe_hits += 1;
            return Some((frame, t));
        }
        self.accurate(target)
    }

    /// 칸 하나: 요청 시각 t(파일 시작 기준)의 프레임. 실패하면 1초 앞에서 한 번 더 시도한다.
    fn cell(
        &mut self,
        t: f64,
        start: f64,
        mode: SeekMode,
        tolerance: f64,
        size: (u32, u32),
    ) -> Frame {
        let mut got = self.grab(start + t, mode, tolerance);
        if got.is_none() {
            got = self.grab(start + (t - 1.0).max(0.0), mode, tolerance);
        }
        match got.and_then(|(frame, ft)| Some((self.frame_image(&frame, size)?, ft))) {
            Some((image, ft)) => Frame {
                time: ft - start,
                image: Some(image),
            },
            None => Frame {
                time: t,
                image: None,
            },
        }
    }

    /// 칸 크기로 줄이고(애너모픽 보정 포함) display matrix대로 돌린다.
    fn frame_image(&mut self, frame: &ffmpeg::frame::Video, size: (u32, u32)) -> Option<RgbImage> {
        let (mut w, mut h) = size;
        if self.rotation == 90 || self.rotation == 270 {
            std::mem::swap(&mut w, &mut h);
        }
        let key = (frame.format(), frame.width(), frame.height(), w, h);
        if self.scaler.as_ref().map(|(_, k)| *k) != Some(key) {
            let ctx = scaling::Context::get(
                key.0,
                key.1,
                key.2,
                Pixel::RGB24,
                w,
                h,
                scaling::Flags::BICUBIC,
            )
            .ok()?;
            self.scaler = Some((ctx, key));
        }
        let mut rgb = ffmpeg::frame::Video::empty();
        self.scaler.as_mut()?.0.run(frame, &mut rgb).ok()?;
        let stride = rgb.stride(0);
        let data = rgb.data(0);
        let mut image = RgbImage::new(w, h);
        for y in 0..h as usize {
            let row = &data[y * stride..y * stride + w as usize * 3];
            image.as_mut()[y * w as usize * 3..(y + 1) * w as usize * 3].copy_from_slice(row);
        }
        Some(match self.rotation {
            90 => image::imageops::rotate270(&image), // 반시계 90° (rotate270 = 시계 270°)
            180 => image::imageops::rotate180(&image),
            270 => image::imageops::rotate90(&image),
            _ => image,
        })
    }
}

/// 시점마다 seek해 칸 크기 프레임을 뽑는다 (spec §7).
///
/// fast 모드에서 키프레임만으로 칸을 채우는 영상은 최대 workers개 스레드가 칸을 나눠 뽑는다.
/// 첫 스레드는 source의 열린 파일을 쓰고, 나머지는 파일을 다시 연다 (여는 데 오래 걸린 파일은
/// 혼자 뽑는다). 어느 스레드가 어떤 순서로 뽑아도 칸마다 고르는 프레임은 같다.
/// 실패한 시점은 1초 앞에서 한 번 더 시도하고, 그래도 실패하면 image=None.
pub fn extract_frames_from(
    source: Source,
    times: &[f64],
    size: (u32, u32),
    mode: SeekMode,
    workers: usize,
) -> ExtractResult {
    let reopen = source.open_seconds <= REOPEN_LIMIT_SECONDS;
    let workers = if reopen { workers } else { 1 };
    let first = Decoding::from_input(source.input, 0, reopen.then_some(source.path.as_path()));
    extract_with(first, &source.path, times, size, mode, workers)
}

/// 파일을 열어 프레임을 뽑는다 (테스트용). 일괄 처리는 extract_frames_from을 쓴다.
#[cfg(test)]
pub fn extract_frames(
    path: &Path,
    times: &[f64],
    size: (u32, u32),
    mode: SeekMode,
    workers: usize,
) -> ExtractResult {
    extract_with(Decoding::open(path, 0), path, times, size, mode, workers)
}

/// 디코더 하나의 스레드 수. 칸을 나눠 뽑으면 코어를 나눠 쓴다 (0 = 자동).
fn decoder_threads(workers: usize) -> usize {
    if workers <= 1 {
        return 0;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cores / workers).max(1)
}

/// 첫 칸을 뽑은 뒤 나머지 칸을 나눠 뽑을 스레드 수 (1이면 나누지 않음).
///
/// 첫 칸을 키프레임 한 장으로 끝냈을 때만 나눈다. 정확한 시점까지 디코딩하는 영상은 디코더가
/// 이미 코어를 다 쓰고, 혼자 이어서 디코딩하는 편이 빠르다 (측정). 남은 칸보다 많이 나누지 않는다.
fn split_workers(requested: usize, cells: usize, first_was_keyframe: bool) -> usize {
    let rest = cells.saturating_sub(1);
    if first_was_keyframe && rest > 0 {
        requested.clamp(1, rest)
    } else {
        1
    }
}

fn extract_with(
    opened: Result<Decoding, ffmpeg::Error>,
    path: &Path,
    times: &[f64],
    size: (u32, u32),
    mode: SeekMode,
    workers: usize,
) -> ExtractResult {
    let mut first = match opened {
        Ok(d) => d,
        Err(e) => {
            return ExtractResult {
                frames: times
                    .iter()
                    .map(|&t| Frame {
                        time: t,
                        image: None,
                    })
                    .collect(),
                warnings: vec![format!("영상을 열 수 없어 모든 칸을 회색으로 둡니다: {e}")],
            };
        }
    };
    let start = first.start_time();
    let tolerance = fast_tolerance(times);

    let mut slots: Vec<Option<Frame>> = vec![None; times.len()];
    let Some(&t0) = times.first() else {
        return ExtractResult {
            frames: vec![],
            warnings: vec![],
        };
    };
    slots[0] = Some(first.cell(t0, start, mode, tolerance, size));
    let workers = split_workers(workers, times.len(), first.keyframe_hits > 0);
    let threads = decoder_threads(workers);
    // 스레드마다 이어진 칸 묶음을 맡는다 (같은 키프레임 구간의 칸은 이어서 디코딩하도록)
    let rest = times.len() - 1;
    let chunk = |k: usize| 1 + k * rest / workers..1 + (k + 1) * rest / workers;
    let work = |decoding: &mut Decoding, range: std::ops::Range<usize>| -> Vec<(usize, Frame)> {
        range
            .map(|i| (i, decoding.cell(times[i], start, mode, tolerance, size)))
            .collect()
    };
    std::thread::scope(|scope| {
        let others: Vec<_> = (1..workers)
            .map(|k| {
                scope.spawn(move || match Decoding::open(path, threads) {
                    Ok(mut d) => work(&mut d, chunk(k)),
                    Err(_) => Vec::new(), // 다시 열지 못한 묶음은 아래에서 첫 스레드가 뽑는다
                })
            })
            .collect();
        let mine = work(&mut first, chunk(0));
        for (i, frame) in others
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .chain(mine)
        {
            slots[i] = Some(frame);
        }
    });
    let frames: Vec<Frame> = slots
        .into_iter()
        .enumerate()
        .map(|(i, f)| f.unwrap_or_else(|| first.cell(times[i], start, mode, tolerance, size)))
        .collect();

    let warnings = frames
        .iter()
        .filter(|f| f.image.is_none())
        .map(|f| {
            format!(
                "{} 프레임을 뽑지 못해 회색 칸으로 둡니다",
                format_duration(f.time)
            )
        })
        .collect();
    ExtractResult { frames, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_media::media;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn plan_reference_and_skips() {
        let plan = plan_times(9717.9, 20, 0.0, 0.0);
        let stamps: Vec<String> = plan.times.iter().map(|&t| format_duration(t)).collect();
        assert_eq!(
            (stamps[0].as_str(), stamps[1].as_str()),
            ("00:07:42", "00:15:25")
        );
        assert_eq!(stamps[19], "02:34:15");
        assert!(plan.warnings.is_empty());

        let p = plan_times(100.0, 4, 0.0, 0.0);
        assert!(
            p.times
                .iter()
                .zip([20.0, 40.0, 60.0, 80.0])
                .all(|(a, b)| approx(*a, b))
        );
        let p = plan_times(100.0, 4, 10.0, 10.0);
        assert!(
            p.times
                .iter()
                .zip([26.0, 42.0, 58.0, 74.0])
                .all(|(a, b)| approx(*a, b))
        );
        assert!(plan_times(10.0, 4, 4.0, 5.0).warnings.is_empty());
        let p = plan_times(10.0, 4, 5.0, 4.7);
        assert!(
            p.times
                .iter()
                .zip([2.0, 4.0, 6.0, 8.0])
                .all(|(a, b)| approx(*a, b))
        );
        assert!(p.warnings[0].contains("무시"));
    }

    #[test]
    fn plan_short_videos() {
        let p = plan_times(1.0, 20, 0.0, 0.0);
        assert_eq!(p.times.len(), 9);
        assert!(
            p.times
                .iter()
                .enumerate()
                .all(|(i, t)| approx(*t, 0.1 * (i + 1) as f64))
        );
        assert_eq!(p.warnings, ["영상이 짧아 20칸 중 9칸만 채웁니다"]);
        assert_eq!(plan_times(2.0, 20, 0.0, 0.0).times.len(), 20);
        let p = plan_times(0.15, 20, 0.0, 0.0);
        assert_eq!(p.times.len(), 1);
        assert!(approx(p.times[0], 0.075));
    }

    #[test]
    fn accurate_20_frames_within_one_frame() {
        let times = plan_times(60.0, 20, 0.0, 0.0).times;
        let r = extract_frames(
            &media("기본_1080p30.mp4"),
            &times,
            (240, 135),
            SeekMode::Accurate,
            1,
        );
        assert!(r.warnings.is_empty());
        for (t, f) in times.iter().zip(&r.frames) {
            let image = f.image.as_ref().unwrap();
            assert_eq!(image.dimensions(), (240, 135));
            assert!(
                *t - 1e-3 <= f.time && f.time <= *t + 1.0 / 30.0 + 1e-3,
                "{t} → {}",
                f.time
            );
        }
    }

    /// 칸마다 seek하고 이어 디코딩도 나눠 뽑기도 하지 않는 기준 결과 (다시 열기 금지)
    fn baseline(path: &Path, times: &[f64], size: (u32, u32), mode: SeekMode) -> ExtractResult {
        probe::init();
        let opened = ffmpeg::format::input(path).unwrap();
        extract_with(
            Decoding::from_input(opened, 0, None),
            path,
            times,
            size,
            mode,
            1,
        )
    }

    #[test]
    fn resume_and_parallel_match_baseline() {
        for (name, duration, mode) in [
            ("기본_1080p30.mp4", 60.0, SeekMode::Fast), // 키프레임 드묾 → 이어 디코딩
            ("기본_1080p30.mp4", 60.0, SeekMode::Accurate),
            ("조각난.mp4", 30.0, SeekMode::Fast), // 키프레임 1초마다 → 칸 나눠 뽑기
            ("조각난.mp4", 30.0, SeekMode::Accurate),
        ] {
            let times = plan_times(duration, 20, 0.0, 0.0).times;
            let base = baseline(&media(name), &times, (160, 90), mode);
            for workers in [1, 4] {
                let r = extract_frames(&media(name), &times, (160, 90), mode, workers);
                assert_eq!(base.warnings, r.warnings);
                for (a, b) in base.frames.iter().zip(&r.frames) {
                    assert_eq!(a.time, b.time, "{name} {mode:?} ×{workers}");
                    assert!(a.image == b.image, "{name} {mode:?} {} 그림이 다름", a.time);
                }
            }
        }
    }

    #[test]
    fn fast_tolerance_is_half_spacing_capped_at_10s() {
        // 60초 영상 20칸: 간격 약 2.86초 → 절반 약 1.43초
        assert!(approx(
            fast_tolerance(&plan_times(60.0, 20, 0.0, 0.0).times),
            60.0 / 21.0 / 2.0
        ));
        // 3시간 27분 영상 20칸: 간격 약 593초 → 절반 약 296초지만 10초로 제한
        assert_eq!(
            fast_tolerance(&plan_times(12452.3, 20, 0.0, 0.0).times),
            10.0
        );
        assert_eq!(fast_tolerance(&[5.0]), 0.0);
    }

    #[test]
    fn fast_uses_nearest_keyframe() {
        // 1초마다 키프레임 (n + 0.023초) → 요청 시각에서 0.5초 안
        let times = plan_times(30.0, 20, 0.0, 0.0).times;
        let r = extract_frames(&media("조각난.mp4"), &times, (320, 180), SeekMode::Fast, 1);
        for (t, f) in times.iter().zip(&r.frames) {
            assert!(f.image.is_some());
            assert!((f.time - t).abs() <= 0.5 + 1e-6, "{t} → {}", f.time);
            assert!(
                ((f.time % 1.0) - 0.023).abs() < 0.01,
                "키프레임 아님: {}",
                f.time
            );
        }
    }

    #[test]
    fn split_workers_never_exceeds_remaining_cells() {
        assert_eq!(split_workers(4, 20, true), 4);
        assert_eq!(split_workers(4, 3, true), 2);
        assert_eq!(split_workers(4, 20, false), 1);
        // 칸이 하나뿐이면 나눌 칸이 없다 (clamp(1, 0)으로 panic하던 경우)
        assert_eq!(split_workers(4, 1, true), 1);
        assert_eq!(split_workers(0, 20, true), 1);
    }

    #[test]
    fn fast_falls_back_when_keyframes_are_sparse() {
        let times = plan_times(10.0, 20, 0.0, 0.0).times;
        let r = extract_frames(&media("무음.mp4"), &times, (160, 120), SeekMode::Fast, 1);
        let got: Vec<f64> = r.frames.iter().map(|f| f.time).collect();
        assert!(r.frames.iter().all(|f| f.image.is_some()));
        assert!(got.windows(2).all(|w| w[0] < w[1]), "순서·겹침: {got:?}");
        let half = (times[1] - times[0]) / 2.0;
        assert!(
            got.iter()
                .zip(&times)
                .all(|(g, t)| (g - t).abs() <= half + 1e-9)
        );
    }

    #[test]
    fn anamorphic_and_rotation_sizes() {
        let r = extract_frames(
            &media("애너모픽.mp4"),
            &[5.0],
            (320, 180),
            SeekMode::Accurate,
            1,
        );
        assert_eq!(r.frames[0].image.as_ref().unwrap().dimensions(), (320, 180));
        let r = extract_frames(
            &media("회전90.mp4"),
            &[2.0],
            (180, 320),
            SeekMode::Accurate,
            1,
        );
        let image = r.frames[0].image.as_ref().unwrap();
        assert_eq!(image.dimensions(), (180, 320));
        // 회전되면 testsrc2의 세로 색 막대가 가로 줄무늬가 된다: 한 행은 거의 같은 색, 한 열은 바뀐다
        let spread = |pixels: Vec<[u8; 3]>| {
            let mean: Vec<f64> = (0..3)
                .map(|c| pixels.iter().map(|p| p[c] as f64).sum::<f64>() / pixels.len() as f64)
                .collect();
            pixels
                .iter()
                .map(|p| (0..3).map(|c| (p[c] as f64 - mean[c]).powi(2)).sum::<f64>())
                .sum::<f64>()
                / pixels.len() as f64
        };
        let row = spread((20..160).map(|x| image.get_pixel(x, 160).0).collect());
        let column = spread((20..300).map(|y| image.get_pixel(90, y).0).collect());
        assert!(row < column, "row {row} column {column}");
    }

    #[test]
    fn failure_retry_broken_and_special_path() {
        let r = extract_frames(
            &media("무음.mp4"),
            &[5.0, 1000.0],
            (160, 120),
            SeekMode::Accurate,
            1,
        );
        assert!(r.frames[0].image.is_some() && r.frames[1].image.is_none());
        assert_eq!(
            r.warnings,
            ["00:16:40 프레임을 뽑지 못해 회색 칸으로 둡니다"]
        );

        let r = extract_frames(
            &media("무음.mp4"),
            &[10.5],
            (160, 120),
            SeekMode::Accurate,
            1,
        );
        assert!(r.frames[0].image.is_some() && r.warnings.is_empty());

        let r = extract_frames(
            &media("깨짐.mp4"),
            &[1.0, 2.0],
            (160, 120),
            SeekMode::Fast,
            1,
        );
        assert!(r.frames.iter().all(|f| f.image.is_none()));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("열 수 없어"));

        let r = extract_frames(
            &media("공백 과 특수문자 (1)&.mp4"),
            &[2.0],
            (160, 120),
            SeekMode::Fast,
            1,
        );
        assert!(r.frames[0].image.is_some());
    }
}
