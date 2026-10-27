//! 시점 계산, 프레임 추출 (spec §7). Python 버전의 PyAV 동작을 FFmpeg 라이브러리로 그대로 옮겼다.

use crate::config::SeekMode;
use crate::formatting::format_duration;
use crate::probe;
use ffmpeg::ffi;
use ffmpeg::software::scaling;
use ffmpeg::util::format::Pixel;
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::path::Path;

/// 칸 사이 최소 간격(초). 이보다 촘촘하면 "매우 짧은 영상"으로 본다.
pub const MIN_SPACING: f64 = 0.1;
/// 프레임 시각 비교 허용 오차(초, 부동소수 오차용)
const EPSILON: f64 = 1e-3;

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

/// swscale 설정 키: (원본 픽셀 형식, 원본 가로, 원본 세로, 출력 가로, 출력 세로)
type ScalerKey = (Pixel, u32, u32, u32, u32);

/// 열린 영상 하나: seek·디코딩을 반복한다 (파일은 한 번만 연다).
struct Decoding {
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    time_base: f64,
    decoder: ffmpeg::decoder::Video,
    eof_sent: bool,
    rotation: i32,
    scaler: Option<(scaling::Context, ScalerKey)>,
}

impl Decoding {
    fn open(path: &Path) -> Result<Self, ffmpeg::Error> {
        probe::init();
        let input = ffmpeg::format::input(path)?;
        let stream = probe::first_video(&input).ok_or(ffmpeg::Error::StreamNotFound)?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = tb.numerator() as f64 / tb.denominator() as f64;
        let rotation =
            ((probe::stream_rotation(&stream) as f64 / 90.0).round() as i32 * 90).rem_euclid(360);
        let mut ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        unsafe {
            // PyAV의 thread_type = "AUTO"와 같게: 프레임·슬라이스 스레드, 개수 자동
            let raw = ctx.as_mut_ptr();
            (*raw).thread_type = ffi::FF_THREAD_FRAME | ffi::FF_THREAD_SLICE;
            (*raw).thread_count = 0;
        }
        let decoder = ctx.decoder().video()?;
        Ok(Decoding {
            input,
            stream_index,
            time_base,
            decoder,
            eof_sent: false,
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
        let ts = (target / self.time_base) as i64; // PyAV int(target / time_base)와 같은 버림
        let flags = if backward {
            ffi::AVSEEK_FLAG_BACKWARD
        } else {
            0
        };
        let ret = unsafe {
            ffi::av_seek_frame(self.input.as_mut_ptr(), self.stream_index as i32, ts, flags)
        };
        if ret < 0 {
            return Err(ffmpeg::Error::from(ret));
        }
        self.decoder.flush();
        self.eof_sent = false;
        Ok(())
    }

    /// 다음 디코딩된 프레임과 그 시각(초). 끝이면 None.
    fn next_frame(&mut self) -> Result<Option<(ffmpeg::frame::Video, f64)>, ffmpeg::Error> {
        let mut frame = ffmpeg::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut frame) {
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
                        let _ = self.decoder.send_packet(&packet);
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    let _ = self.decoder.send_eof();
                    self.eof_sent = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// target 직전/직후 키프레임으로 seek해 첫 프레임 하나만 디코딩한다. 그 방향에 없으면 None.
    fn keyframe(&mut self, target: f64, backward: bool) -> Option<(ffmpeg::frame::Video, f64)> {
        self.seek(target, backward).ok()?;
        self.next_frame().ok().flatten()
    }

    /// target 이전 키프레임부터 디코딩해 target 이상인 첫 프레임 (ffmpeg 입력 seek과 같음).
    fn accurate(&mut self, target: f64) -> Option<(ffmpeg::frame::Video, f64)> {
        self.seek(target, true).ok()?;
        while let Ok(Some((frame, t))) = self.next_frame() {
            if t >= target - EPSILON {
                return Some((frame, t));
            }
        }
        None // target 이후 프레임이 없음 (끝을 넘음)
    }

    /// target 앞뒤 키프레임 중 더 가까운 쪽 (디코딩은 2프레임뿐).
    fn nearest_keyframe(&mut self, target: f64) -> Option<(ffmpeg::frame::Video, f64)> {
        let before = self.keyframe(target, true);
        let after = self.keyframe(target, false);
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
    /// fast: 가장 가까운 키프레임이 tolerance(칸 간격의 절반) 안이면 그것, 더 멀면 정확하게.
    fn grab(
        &mut self,
        target: f64,
        mode: SeekMode,
        tolerance: f64,
    ) -> Option<(ffmpeg::frame::Video, f64)> {
        if mode == SeekMode::Fast
            && let Some((frame, t)) = self.nearest_keyframe(target)
            && (t - target).abs() <= tolerance
        {
            return Some((frame, t));
        }
        self.accurate(target)
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

/// 파일을 한 번 열고 시점마다 seek해 칸 크기 프레임을 뽑는다 (spec §7).
///
/// 실패한 시점은 1초 앞에서 한 번 더 시도하고, 그래도 실패하면 image=None.
pub fn extract_frames(
    path: &Path,
    times: &[f64],
    size: (u32, u32),
    mode: SeekMode,
) -> ExtractResult {
    let mut decoding = match Decoding::open(path) {
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
    let start = decoding.start_time();
    // fast 모드 허용 오차: 칸 간격의 절반 (칸이 하나면 정확하게)
    let tolerance = if times.len() > 1 {
        (times[1] - times[0]) / 2.0
    } else {
        0.0
    };

    let mut frames = Vec::with_capacity(times.len());
    for &t in times {
        let mut got = decoding.grab(start + t, mode, tolerance);
        if got.is_none() {
            got = decoding.grab(start + (t - 1.0).max(0.0), mode, tolerance);
        }
        let frame = match got {
            Some((frame, ft)) => match decoding.frame_image(&frame, size) {
                Some(image) => Frame {
                    time: ft - start,
                    image: Some(image),
                },
                None => Frame {
                    time: t,
                    image: None,
                },
            },
            None => Frame {
                time: t,
                image: None,
            },
        };
        frames.push(frame);
    }

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

    #[test]
    fn fast_uses_nearest_keyframe() {
        // 1초마다 키프레임 (n + 0.023초) → 요청 시각에서 0.5초 안
        let times = plan_times(30.0, 20, 0.0, 0.0).times;
        let r = extract_frames(&media("조각난.mp4"), &times, (320, 180), SeekMode::Fast);
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
    fn fast_falls_back_when_keyframes_are_sparse() {
        let times = plan_times(10.0, 20, 0.0, 0.0).times;
        let r = extract_frames(&media("무음.mp4"), &times, (160, 120), SeekMode::Fast);
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
        );
        assert_eq!(r.frames[0].image.as_ref().unwrap().dimensions(), (320, 180));
        let r = extract_frames(&media("회전90.mp4"), &[2.0], (180, 320), SeekMode::Accurate);
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
        );
        assert!(r.frames[0].image.is_some() && r.frames[1].image.is_none());
        assert_eq!(
            r.warnings,
            ["00:16:40 프레임을 뽑지 못해 회색 칸으로 둡니다"]
        );

        let r = extract_frames(&media("무음.mp4"), &[10.5], (160, 120), SeekMode::Accurate);
        assert!(r.frames[0].image.is_some() && r.warnings.is_empty());

        let r = extract_frames(&media("깨짐.mp4"), &[1.0, 2.0], (160, 120), SeekMode::Fast);
        assert!(r.frames.iter().all(|f| f.image.is_none()));
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("열 수 없어"));

        let r = extract_frames(
            &media("공백 과 특수문자 (1)&.mp4"),
            &[2.0],
            (160, 120),
            SeekMode::Fast,
        );
        assert!(r.frames[0].image.is_some());
    }
}
