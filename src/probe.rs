//! 영상 정보 읽기 → VideoInfo (spec §6). Python 버전의 ffprobe 대신 FFmpeg 라이브러리를 직접 쓴다.
//!
//! 값은 ffprobe가 내는 것과 같게 계산한다 (codec_tag_string, SAR/DAR, rotation, duration).

use ffmpeg::ffi;
use ffmpeg::format::stream::Disposition;
use ffmpeg::media::Type;
use ffmpeg_next as ffmpeg;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub size_bytes: u64,
    pub width: u32,      // 회전 적용
    pub height: u32,     // 회전 적용
    pub dar: (u32, u32), // 회전 적용
    pub fps: Option<f64>,
    pub video_codec_name: String,
    pub video_codec_tag: String,
    pub audio_codec_name: Option<String>,
    pub duration: f64,
    pub rotation: i32, // display matrix 회전 (반시계, 0/90/180/270)
}

impl VideoInfo {
    /// 표시 비율(가로/세로). 애너모픽 보정이 반영된 값.
    pub fn aspect(&self) -> f64 {
        self.dar.0 as f64 / self.dar.1 as f64
    }
}

#[derive(Debug, PartialEq)]
pub struct ProbeError(pub String);

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

/// FFmpeg 초기화 (여러 번 불러도 된다). 로그는 끈다: 오류는 Result로 받는다.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = ffmpeg::init();
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Quiet);
    });
}

/// 파일 읽기 버퍼(바이트). 색인 없는 fragmented MP4는 조각마다 seek하므로 작을수록 덜 읽는다
/// (reference2 열기: 32KB 0.43초, 4KB 0.36초).
const IO_BUFFER_SIZE: usize = 4096;

/// 위치와 크기를 직접 기억하는 파일. FFmpeg는 상자마다 파일 크기·위치를 묻고 seek하는데,
/// 실제 이동은 다음 read 직전에 한 번만 한다 (조각 13,439개 파일에서 시스템 호출을 크게 줄임).
struct LazySeekFile {
    file: std::fs::File,
    size: u64,
    pos: u64,  // FFmpeg가 보는 위치
    real: u64, // 파일 핸들의 실제 위치
}

impl LazySeekFile {
    fn new(file: std::fs::File) -> std::io::Result<Self> {
        let size = file.metadata()?.len();
        Ok(LazySeekFile {
            file,
            size,
            pos: 0,
            real: 0,
        })
    }
}

impl std::io::Read for LazySeekFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Seek;
        if self.real != self.pos {
            self.file.seek(std::io::SeekFrom::Start(self.pos))?;
            self.real = self.pos;
        }
        let n = self.file.read(buf)?;
        self.pos += n as u64;
        self.real = self.pos;
        Ok(n)
    }
}

impl std::io::Seek for LazySeekFile {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            std::io::SeekFrom::Start(p) => Some(p),
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            std::io::SeekFrom::End(d) => self.size.checked_add_signed(d),
        };
        self.pos = target.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(self.pos)
    }
}

/// 영상 파일을 연다 (avformat_open_input + find_stream_info, ffmpeg::format::input과 같음).
///
/// 파일 바이트는 FFmpeg 기본 파일 읽기 대신 Rust 파일 읽기로 넘긴다. 분석은 FFmpeg가 그대로
/// 하므로 결과는 같고, Windows에서 조각 많은 MP4를 열 때 훨씬 빠르다 (reference2 1.40초 → 0.36초).
pub fn open_input(path: &Path) -> Result<ffmpeg::format::context::Input, ffmpeg::Error> {
    init();
    let file = std::fs::File::open(path).map_err(|e| ffmpeg::Error::Other {
        errno: match e.kind() {
            std::io::ErrorKind::NotFound => ffi::ENOENT,
            std::io::ErrorKind::PermissionDenied => ffi::EACCES,
            _ => ffi::EIO,
        },
    })?;
    let file = LazySeekFile::new(file).map_err(|_| ffmpeg::Error::Other { errno: ffi::EIO })?;
    let io = ffmpeg::format::context::StreamIo::from_read_seek_with_capacity(file, IO_BUFFER_SIZE)?;
    // 이름은 형식 판별(확장자)에 쓰인다: 기본 열기에 넘기던 경로 그대로
    ffmpeg::format::input_from_stream(io, Some(&path.to_string_lossy()), None)
}

/// -v 출력용: FFmpeg 라이브러리 버전과 구성
pub fn backend_version() -> String {
    let v = unsafe { ffi::avformat_version() };
    let config = unsafe { std::ffi::CStr::from_ptr(ffi::avcodec_license()) }
        .to_string_lossy()
        .into_owned();
    format!(
        "FFmpeg 라이브러리 (exe 내장, libavformat {}.{}.{}, {config})",
        v >> 16,
        (v >> 8) & 0xff,
        v & 0xff
    )
}

/// ffprobe의 codec_tag_string과 같다 (av_fourcc_make_string: 영숫자·". -_" 외에는 [숫자]).
fn fourcc_string(tag: u32) -> String {
    tag.to_le_bytes()
        .iter()
        .map(|&b| {
            let c = b as char;
            if c.is_ascii_alphanumeric() || ". -_".contains(c) {
                c.to_string()
            } else {
                format!("[{b}]")
            }
        })
        .collect()
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn reduce(num: u64, den: u64) -> (u32, u32) {
    let g = gcd(num, den).max(1);
    ((num / g) as u32, (den / g) as u32)
}

fn positive_rate(r: ffmpeg::Rational) -> Option<f64> {
    (r.numerator() > 0 && r.denominator() > 0)
        .then(|| r.numerator() as f64 / r.denominator() as f64)
}

/// 스트림 codecpar의 display matrix 회전(반시계 각도). ffprobe의 rotation과 같다.
pub fn stream_rotation(stream: &ffmpeg::format::stream::Stream) -> i32 {
    unsafe {
        let par = (*stream.as_ptr()).codecpar;
        let side = ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if side.is_null() || (*side).size < 36 {
            return 0;
        }
        let degrees = ffi::av_display_rotation_get((*side).data as *const i32);
        if degrees.is_finite() {
            degrees.round() as i32
        } else {
            0
        }
    }
}

/// 첫 비디오 스트림 (앨범 아트 attached_pic 제외)
pub fn first_video(
    input: &ffmpeg::format::context::Input,
) -> Option<ffmpeg::format::stream::Stream<'_>> {
    input.streams().find(|s| {
        s.parameters().medium() == Type::Video
            && !s.disposition().contains(Disposition::ATTACHED_PIC)
    })
}

/// 영상 정보를 읽는다. 실패하면 ProbeError (메시지는 사용자에게 그대로 보여준다).
#[cfg(test)]
pub fn probe(path: &Path) -> Result<VideoInfo, ProbeError> {
    open(path).map(|(info, _)| info)
}

/// 영상을 열어 정보를 읽고, 연 파일도 함께 돌려준다 (추출에서 다시 열지 않도록).
/// 인덱스 없는 fragmented MP4는 열 때마다 조각 헤더 전체를 훑는다 (reference2 약 0.4초).
pub fn open(path: &Path) -> Result<(VideoInfo, ffmpeg::format::context::Input), ProbeError> {
    let size_bytes = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| ProbeError(format!("영상을 읽을 수 없음: {e}")))?;
    let input = open_input(path).map_err(|e| ProbeError(format!("영상을 읽을 수 없음: {e}")))?;
    let info = read_info(path, size_bytes, &input)?;
    Ok((info, input))
}

fn read_info(
    path: &Path,
    size_bytes: u64,
    input: &ffmpeg::format::context::Input,
) -> Result<VideoInfo, ProbeError> {
    let video = first_video(input).ok_or_else(|| ProbeError("비디오 스트림 없음".into()))?;
    let audio = input
        .streams()
        .find(|s| s.parameters().medium() == Type::Audio);

    let (mut width, mut height, tag, sar) = unsafe {
        let par = *(*video.as_ptr()).codecpar;
        let sar = ffi::av_guess_sample_aspect_ratio(
            input.as_ptr() as *mut _,
            video.as_ptr() as *mut _,
            std::ptr::null_mut(),
        );
        (
            par.width.max(0) as u64,
            par.height.max(0) as u64,
            par.codec_tag,
            sar,
        )
    };
    if width == 0 || height == 0 {
        return Err(ProbeError("해상도를 알 수 없음".into()));
    }
    // ffprobe display_aspect_ratio = 저장 크기 × SAR. SAR을 모르면 저장 크기만으로 약분
    let mut dar = if sar.num > 0 && sar.den > 0 {
        reduce(width * sar.num as u64, height * sar.den as u64)
    } else {
        reduce(width, height)
    };

    let rotation = (stream_rotation(&video) as f64 / 90.0).round() as i32 * 90;
    let rotation = rotation.rem_euclid(360);
    if rotation % 180 == 90 {
        std::mem::swap(&mut width, &mut height);
        dar = (dar.1, dar.0);
    }

    let format_duration = input.duration();
    let mut duration =
        (format_duration > 0).then(|| format_duration as f64 / ffi::AV_TIME_BASE as f64);
    if duration.is_none() && video.duration() > 0 {
        let tb = video.time_base();
        duration = Some(video.duration() as f64 * tb.numerator() as f64 / tb.denominator() as f64);
    }
    let duration = duration
        .filter(|d| d.is_finite() && *d > 0.0)
        .ok_or_else(|| ProbeError("길이를 알 수 없음".into()))?;

    Ok(VideoInfo {
        path: path.to_path_buf(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size_bytes,
        width: width as u32,
        height: height as u32,
        dar,
        fps: positive_rate(video.avg_frame_rate()).or_else(|| positive_rate(video.rate())),
        video_codec_name: video.parameters().id().name().to_string(),
        video_codec_tag: fourcc_string(tag),
        audio_codec_name: audio.map(|a| a.parameters().id().name().to_string()),
        duration,
        rotation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HeaderSettings, Language};
    use crate::formatting::header_lines;
    use crate::test_media::media;

    #[test]
    fn rust_file_reading_matches_ffmpeg_file_reading() {
        // 같은 파일을 FFmpeg 기본 파일 읽기로 연 결과와 정보·시작 시각·스트림 길이가 같아야 한다
        for name in [
            "기본_1080p30.mp4",
            "세로_720x1280.mp4",
            "회전90.mp4",
            "애너모픽.mp4",
            "조각난.mp4",
            "짧음_1초.mp4",
            "가변프레임.mkv",
            "무음.mp4",
            "공백 과 특수문자 (1)&.mp4",
        ] {
            let path = media(name);
            let size = std::fs::metadata(&path).unwrap().len();
            let ours = open_input(&path).unwrap();
            let theirs = ffmpeg::format::input(&path).unwrap();
            assert_eq!(
                read_info(&path, size, &ours),
                read_info(&path, size, &theirs),
                "{name}"
            );
            let timing = |i: &ffmpeg::format::context::Input| {
                let start = unsafe { (*i.as_ptr()).start_time };
                let streams: Vec<_> = i
                    .streams()
                    .map(|s| (s.start_time(), s.duration(), s.avg_frame_rate(), s.rate()))
                    .collect();
                (start, i.duration(), streams)
            };
            assert_eq!(timing(&ours), timing(&theirs), "{name}");
        }
        assert!(open_input(&media("깨짐.mp4")).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(open_input(&dir.path().join("없는 파일.mp4")).is_err());
    }

    #[test]
    fn fourcc() {
        assert_eq!(fourcc_string(u32::from_le_bytes(*b"avc1")), "avc1");
        assert_eq!(fourcc_string(0), "[0][0][0][0]");
        assert_eq!(fourcc_string(27), "[27][0][0][0]");
    }

    /// Python tests/test_probe.py의 EXPECTED와 같은 값
    /// (이름, 가로, 세로, DAR, fps, 비디오 코덱, 오디오 코덱, 재생 시간)
    type Expected = (
        &'static str,
        u32,
        u32,
        (u32, u32),
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    );

    #[test]
    fn media_matches_python_expectations() {
        let cases: [Expected; 8] = [
            (
                "기본_1080p30.mp4",
                1920,
                1080,
                (16, 9),
                "30",
                "AVC1",
                "AAC",
                "00:01:00",
            ),
            (
                "무음.mp4",
                320,
                240,
                (4, 3),
                "25",
                "AVC1",
                "None",
                "00:00:10",
            ),
            (
                "세로_720x1280.mp4",
                720,
                1280,
                (9, 16),
                "30",
                "AVC1",
                "AAC",
                "00:00:10",
            ),
            (
                "회전90.mp4",
                360,
                640,
                (9, 16),
                "25",
                "AVC1",
                "AAC",
                "00:00:05",
            ),
            (
                "짧음_1초.mp4",
                320,
                240,
                (4, 3),
                "30",
                "AVC1",
                "AAC",
                "00:00:01",
            ),
            (
                "가변프레임.mkv",
                320,
                240,
                (4, 3),
                "30",
                "H264",
                "AAC",
                "00:00:10",
            ),
            (
                "애너모픽.mp4",
                640,
                480,
                (16, 9),
                "25",
                "AVC1",
                "AAC",
                "00:00:10",
            ),
            (
                "공백 과 특수문자 (1)&.mp4",
                320,
                240,
                (4, 3),
                "25",
                "AVC1",
                "AAC",
                "00:00:05",
            ),
        ];
        for (name, w, h, dar, fps, vcodec, acodec, duration) in cases {
            let path = media(name);
            let info = probe(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(info.file_name, name);
            assert_eq!(info.size_bytes, std::fs::metadata(&path).unwrap().len());
            assert_eq!((info.width, info.height, info.dar), (w, h, dar), "{name}");
            let lines = header_lines(&info, &HeaderSettings::default(), Language::En);
            assert_eq!(lines[0], format!("File Name: {name}"));
            assert_eq!(
                lines[2],
                format!("Resolution: {w}×{h}({}:{}), fps: {fps}", dar.0, dar.1),
                "{name}"
            );
            assert_eq!(
                lines[3],
                format!("Video Codec: {vcodec}, Audio Codec: {acodec}"),
                "{name}"
            );
            assert_eq!(lines[4], format!("Duration: {duration}"), "{name}");
        }
    }

    #[test]
    fn anamorphic_aspect_and_rotation() {
        assert!((probe(&media("애너모픽.mp4")).unwrap().aspect() - 16.0 / 9.0).abs() < 1e-9);
        assert_eq!(probe(&media("회전90.mp4")).unwrap().rotation, 90);
    }

    #[test]
    fn broken_and_not_video() {
        let err = probe(&media("깨짐.mp4")).unwrap_err();
        assert!(err.0.starts_with("영상을 읽을 수 없음"), "{}", err.0);
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("메모.txt");
        std::fs::write(&text, "영상 아님").unwrap();
        assert!(probe(&text).is_err());
    }
}
