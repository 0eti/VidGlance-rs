//! 크기·시간·비율·코덱 문자열과 헤더 줄 구성 (spec §6).
//!
//! 헤더 레이블 문자열은 이 모듈의 `labels()` 한곳에만 둔다.

use crate::config::{HeaderSettings, Language};
use crate::probe::VideoInfo;

pub struct Labels {
    pub file_name: &'static str,
    pub size: &'static str,
    pub bytes: &'static str,
    pub resolution: &'static str,
    pub fps: &'static str,
    pub video_codec: &'static str,
    pub audio_codec: &'static str,
    pub no_audio: &'static str,
    pub duration: &'static str,
}

const EN: Labels = Labels {
    file_name: "File Name",
    size: "Size",
    bytes: "bytes",
    resolution: "Resolution",
    fps: "fps",
    video_codec: "Video Codec",
    audio_codec: "Audio Codec",
    no_audio: "None",
    duration: "Duration",
};

const KO: Labels = Labels {
    file_name: "파일명",
    size: "크기",
    bytes: "바이트",
    resolution: "해상도",
    fps: "fps",
    video_codec: "비디오 코덱",
    audio_codec: "오디오 코덱",
    no_audio: "없음",
    duration: "재생 시간",
};

pub fn labels(language: Language) -> &'static Labels {
    match language {
        Language::En => &EN,
        Language::Ko => &KO,
    }
}

const SIZE_UNITS: [&str; 3] = ["KB", "MB", "GB"];

/// 소수 둘째 자리로 반올림한 값 (표시되는 값과 같게 문자열로 반올림한다)
fn rounded2(value: f64) -> f64 {
    format!("{value:.2}").parse().unwrap_or(value)
}

/// 1024 기준, 소수 둘째 자리. 예: 7438762215 → `6.93GB`.
pub fn format_size(size_bytes: u64) -> String {
    let mut value = size_bytes as f64 / 1024.0;
    let mut unit = 0;
    // 반올림한 값이 1024가 되면 다음 단위로 올린다 (1024.00KB 대신 1.00MB)
    while rounded2(value) >= 1024.0 && unit < SIZE_UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2}{}", SIZE_UNITS[unit])
}

/// 1234567 → `1,234,567`
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 초 → `HH:MM:SS` (초 아래 내림, 100시간 이상이면 시 자리가 늘어남).
pub fn format_duration(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds.floor() as u64
    } else {
        0
    };
    let (hours, rest) = (total / 3600, total % 3600);
    let (minutes, secs) = (rest / 60, rest % 60);
    format!("{hours:02}:{minutes:02}:{secs:02}")
}

/// 소수 둘째 자리, 끝자리 0 제거. 예: 59.7700 → `59.77`, 30.0 → `30`.
pub fn format_fps(fps: Option<f64>) -> String {
    match fps {
        None => "N/A".to_string(),
        Some(v) => {
            let s = format!("{v:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    }
}

/// 예: `1920×1080(16:9)`.
pub fn format_resolution(width: u32, height: u32, dar: (u32, u32)) -> String {
    format!("{width}×{height}({}:{})", dar.0, dar.1)
}

/// codec_tag가 의미 있으면 그것을, 아니면 codec_name을 대문자로.
pub fn video_codec_label(codec_name: &str, codec_tag: &str) -> String {
    let meaningful = !codec_tag.is_empty()
        && !codec_tag.contains('[')
        && codec_tag.chars().all(|c| !c.is_control());
    if meaningful {
        codec_tag.to_uppercase()
    } else {
        codec_name.to_uppercase()
    }
}

/// 오디오는 항상 codec_name 대문자. 오디오가 없으면 None/없음.
pub fn audio_codec_label(codec_name: Option<&str>, language: Language) -> String {
    match codec_name {
        None => labels(language).no_audio.to_string(),
        Some(name) => name.to_uppercase(),
    }
}

/// 켜진 항목만 모아 표시할 헤더 줄 목록을 만든다. 모두 꺼지면 빈 목록.
pub fn header_lines(info: &VideoInfo, header: &HeaderSettings, language: Language) -> Vec<String> {
    let l = labels(language);
    let size = format_size(info.size_bytes);
    let resolution = format_resolution(info.width, info.height, info.dar);
    let video = video_codec_label(&info.video_codec_name, &info.video_codec_tag);
    let audio = audio_codec_label(info.audio_codec_name.as_deref(), language);
    let duration = format_duration(info.duration);

    // 줄마다 (켜짐 여부, 문자열) 목록. 같은 줄의 켜진 항목은 ", "로 잇는다.
    let rows: [Vec<(bool, String)>; 5] = [
        vec![(
            header.file_name,
            format!("{}: {}", l.file_name, info.file_name),
        )],
        vec![(
            header.size,
            format!(
                "{}: {size} ({} {})",
                l.size,
                group_thousands(info.size_bytes),
                l.bytes
            ),
        )],
        vec![
            (header.resolution, format!("{}: {resolution}", l.resolution)),
            (header.fps, format!("{}: {}", l.fps, format_fps(info.fps))),
        ],
        vec![
            (header.video_codec, format!("{}: {video}", l.video_codec)),
            (header.audio_codec, format!("{}: {audio}", l.audio_codec)),
        ],
        vec![(header.duration, format!("{}: {duration}", l.duration))],
    ];
    rows.into_iter()
        .filter_map(|row| {
            let parts: Vec<String> = row
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, s)| s)
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 레퍼런스 시트 헤더에 나온 예시 값 (Python tests/test_formatting.py와 같음)
    fn reference() -> VideoInfo {
        VideoInfo {
            path: PathBuf::from("예시영상.mp4"),
            file_name: "예시영상.mp4".into(),
            size_bytes: 7_438_762_215,
            width: 1920,
            height: 1080,
            dar: (16, 9),
            fps: Some(59.77),
            video_codec_name: "h264".into(),
            video_codec_tag: "avc1".into(),
            audio_codec_name: Some("aac".into()),
            duration: 9717.4,
            rotation: 0,
        }
    }

    #[test]
    fn size() {
        for (bytes, expected) in [
            (0, "0.00KB"),
            (512, "0.50KB"),
            (1024, "1.00KB"),
            (1024 * 1023, "1023.00KB"),
            (1024 * 1024 - 1, "1.00MB"),
            (1024 * 1024, "1.00MB"),
            (1024u64.pow(3) - 1, "1.00GB"),
            (7_438_762_215, "6.93GB"),
            (1500 * 1024u64.pow(3), "1500.00GB"),
        ] {
            assert_eq!(format_size(bytes), expected, "{bytes}");
        }
    }

    #[test]
    fn thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(7_438_762_215), "7,438,762,215");
    }

    #[test]
    fn duration() {
        for (s, expected) in [
            (0.0, "00:00:00"),
            (-1.0, "00:00:00"),
            (59.999, "00:00:59"),
            (60.0, "00:01:00"),
            (3599.9, "00:59:59"),
            (9717.9, "02:41:57"),
            (360_000.0, "100:00:00"),
        ] {
            assert_eq!(format_duration(s), expected, "{s}");
        }
    }

    #[test]
    fn fps() {
        for (v, expected) in [
            (Some(59.77), "59.77"),
            (Some(30.0), "30"),
            (Some(30000.0 / 1001.0), "29.97"),
            (Some(24000.0 / 1001.0), "23.98"),
            (Some(25.5), "25.5"),
            (Some(25.001), "25"),
            (None, "N/A"),
        ] {
            assert_eq!(format_fps(v), expected);
        }
    }

    #[test]
    fn codec_labels() {
        assert_eq!(video_codec_label("h264", "avc1"), "AVC1");
        assert_eq!(video_codec_label("hevc", "hvc1"), "HVC1");
        assert_eq!(video_codec_label("h264", "[0][0][0][0]"), "H264");
        assert_eq!(video_codec_label("h264", "[27][0][0][0]"), "H264");
        assert_eq!(video_codec_label("vp9", ""), "VP9");
        assert_eq!(audio_codec_label(Some("aac"), Language::En), "AAC");
        assert_eq!(audio_codec_label(None, Language::En), "None");
        assert_eq!(audio_codec_label(None, Language::Ko), "없음");
    }

    #[test]
    fn header_reference_en_ko() {
        assert_eq!(
            header_lines(&reference(), &HeaderSettings::default(), Language::En),
            [
                "File Name: 예시영상.mp4",
                "Size: 6.93GB (7,438,762,215 bytes)",
                "Resolution: 1920×1080(16:9), fps: 59.77",
                "Video Codec: AVC1, Audio Codec: AAC",
                "Duration: 02:41:57",
            ]
        );
        assert_eq!(
            header_lines(&reference(), &HeaderSettings::default(), Language::Ko),
            [
                "파일명: 예시영상.mp4",
                "크기: 6.93GB (7,438,762,215 바이트)",
                "해상도: 1920×1080(16:9), fps: 59.77",
                "비디오 코덱: AVC1, 오디오 코덱: AAC",
                "재생 시간: 02:41:57",
            ]
        );
    }

    #[test]
    fn header_combinations() {
        let off = HeaderSettings::all(false);
        let cases: [(HeaderSettings, &[&str]); 6] = [
            (HeaderSettings { fps: true, ..off }, &["fps: 59.77"]),
            (
                HeaderSettings {
                    resolution: true,
                    ..off
                },
                &["Resolution: 1920×1080(16:9)"],
            ),
            (
                HeaderSettings {
                    video_codec: true,
                    ..off
                },
                &["Video Codec: AVC1"],
            ),
            (
                HeaderSettings {
                    audio_codec: true,
                    ..off
                },
                &["Audio Codec: AAC"],
            ),
            (
                HeaderSettings {
                    file_name: true,
                    duration: true,
                    ..off
                },
                &["File Name: 예시영상.mp4", "Duration: 02:41:57"],
            ),
            (off, &[]),
        ];
        for (header, expected) in cases {
            assert_eq!(header_lines(&reference(), &header, Language::En), expected);
        }
        let mut silent = reference();
        silent.audio_codec_name = None;
        assert_eq!(
            header_lines(&silent, &HeaderSettings::default(), Language::Ko)[3],
            "비디오 코덱: AVC1, 오디오 코덱: 없음"
        );
    }
}
