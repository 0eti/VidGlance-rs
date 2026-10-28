//! 시트 합성, 저장 (spec §8, §9). Python 버전의 Pillow 그리기를 같은 좌표 규칙으로 옮겼다.

use crate::config::{OutputFormat, ThemeName, TimestampPosition};
use crate::extract::Frame;
use crate::fonts::FontFiles;
use crate::formatting::format_duration;
use crate::layout::{Layout, Rect};
use ab_glyph::{Font, FontArc, GlyphId, OutlinedGlyph, PxScale, ScaleFont, point};
use image::{ImageEncoder, RgbImage};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

type Rgb = [u8; 3];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Rgb,
    pub header_text: Rgb,
    pub cell_background: Rgb, // 레터박스
    pub failed_cell: Rgb,
}

pub fn theme(name: ThemeName) -> Theme {
    match name {
        ThemeName::Light => Theme {
            background: [0xFF, 0xFF, 0xFF],
            header_text: [0x00, 0x00, 0x00],
            cell_background: [0x00, 0x00, 0x00],
            failed_cell: [0x9E, 0x9E, 0x9E],
        },
        ThemeName::Dark => Theme {
            background: [0x1E, 0x1E, 0x1E],
            header_text: [0xED, 0xED, 0xED],
            cell_background: [0x00, 0x00, 0x00],
            failed_cell: [0x4A, 0x4A, 0x4A],
        },
    }
}

// 타임스탬프는 테마와 관계없이 같은 스타일 (밝은 장면에서도 읽히도록): 검정 60% 박스 + 흰 글자
const TIMESTAMP_BOX_ALPHA: u32 = 153; // round(255 × 0.6)
const TIMESTAMP_TEXT: Rgb = [0xFF, 0xFF, 0xFF];
const ELLIPSIS: char = '…';

/// 저장 중인 임시 파일들 (영상을 동시에 처리하면 여럿). 콘솔 창을 닫을 때 app.rs가 지운다.
pub static SAVING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Pillow와 같은 크기 규칙의 글꼴: size는 em 크기(px)다.
pub struct Text<'a> {
    font: &'a FontArc,
    scale: PxScale,
}

impl<'a> Text<'a> {
    pub fn new(font: &'a FontArc, size_px: f32) -> Self {
        // ab_glyph의 PxScale은 (ascent − descent) 높이, Pillow 크기는 em 크기 → 환산
        let upem = font.units_per_em().unwrap_or(1000.0);
        Text {
            font,
            scale: PxScale::from(size_px * font.height_unscaled() / upem),
        }
    }

    fn ascent(&self) -> f32 {
        self.font.as_scaled(self.scale).ascent()
    }

    fn descent(&self) -> f32 {
        self.font.as_scaled(self.scale).descent() // 음수
    }

    /// 글자마다 (글리프, 시작 x). 커닝 포함.
    fn layout(&self, text: &str) -> (Vec<(GlyphId, f32)>, f32) {
        let scaled = self.font.as_scaled(self.scale);
        let mut x = 0.0;
        let mut prev: Option<GlyphId> = None;
        let mut glyphs = Vec::new();
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(p) = prev {
                x += scaled.kern(p, id);
            }
            glyphs.push((id, x));
            x += scaled.h_advance(id);
            prev = Some(id);
        }
        (glyphs, x)
    }

    /// Pillow getlength와 같은 뜻: 글자 진행 폭
    pub fn length(&self, text: &str) -> f32 {
        self.layout(text).1
    }

    fn outlines(&self, text: &str, origin_x: f32, baseline: f32) -> Vec<OutlinedGlyph> {
        self.layout(text)
            .0
            .into_iter()
            .filter_map(|(id, x)| {
                self.font.outline_glyph(
                    id.with_scale_and_position(self.scale, point(origin_x + x, baseline)),
                )
            })
            .collect()
    }

    /// 잉크 영역 (원점 x=0, 기준선 y=0 기준): (left, top, right, bottom)
    fn ink_bounds(&self, text: &str) -> (f32, f32, f32, f32) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for g in self.outlines(text, 0.0, 0.0) {
            let r = g.px_bounds();
            b = (
                b.0.min(r.min.x),
                b.1.min(r.min.y),
                b.2.max(r.max.x),
                b.3.max(r.max.y),
            );
        }
        if b.0 > b.2 { (0.0, 0.0, 0.0, 0.0) } else { b }
    }

    /// 기준선 위치에 글자를 그린다 (덮개 비율만큼 색을 섞는다).
    fn draw(&self, image: &mut RgbImage, origin_x: f32, baseline: f32, text: &str, color: Rgb) {
        let (w, h) = image.dimensions();
        for g in self.outlines(text, origin_x, baseline) {
            let r = g.px_bounds();
            g.draw(|gx, gy, coverage| {
                let x = r.min.x as i64 + gx as i64;
                let y = r.min.y as i64 + gy as i64;
                if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 || coverage <= 0.0 {
                    return;
                }
                let a = coverage.min(1.0);
                let p = image.get_pixel_mut(x as u32, y as u32);
                for (channel, target) in p.0.iter_mut().zip(color) {
                    *channel = (*channel as f32 * (1.0 - a) + target as f32 * a).round() as u8;
                }
            });
        }
    }
}

/// 너비를 넘으면 가운데를 `…`로 줄인다 (앞뒤를 최대한 남긴다).
pub fn fit_text(text: &str, font: &Text, max_width: f32) -> String {
    if font.length(text) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let shortened = |keep: usize| -> String {
        let head = keep - keep / 2;
        let tail = keep / 2;
        let mut s: String = chars[..head].iter().collect();
        s.push(ELLIPSIS);
        s.extend(&chars[chars.len() - tail..]);
        s
    };
    // 남길 글자 수를 이분 탐색
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if font.length(&shortened(mid)) <= max_width {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    shortened(lo)
}

fn fill(image: &mut RgbImage, x0: i64, y0: i64, x1: i64, y1: i64, color: Rgb) {
    let (w, h) = image.dimensions();
    for y in y0.max(0)..y1.min(h as i64) {
        for x in x0.max(0)..x1.min(w as i64) {
            image.put_pixel(x as u32, y as u32, image::Rgb(color));
        }
    }
}

fn darken(image: &mut RgbImage, x0: i64, y0: i64, x1: i64, y1: i64, alpha: u32) {
    let (w, h) = image.dimensions();
    for y in y0.max(0)..y1.min(h as i64) {
        for x in x0.max(0)..x1.min(w as i64) {
            let p = image.get_pixel_mut(x as u32, y as u32);
            for c in 0..3 {
                p.0[c] = ((p.0[c] as u32 * (255 - alpha) + 127) / 255) as u8;
            }
        }
    }
}

/// 칸 안 position 자리에 반투명 박스 + 흰 글자. 모서리는 칸 가장자리에서 margin만큼 띄운다.
fn draw_timestamp(
    image: &mut RgbImage,
    rect: Rect,
    text: &str,
    font: &Text,
    layout: &Layout,
    position: TimestampPosition,
) {
    // Pillow textbbox((0,0), anchor="lt"): 위쪽 기준은 어센더 선
    let ascent = font.ascent();
    let (l, t, r, b) = font.ink_bounds(text);
    let (left, top, right, bottom) = (
        l.floor(),
        (ascent + t).floor(),
        r.ceil(),
        (ascent + b).ceil(),
    );
    let pad = layout.timestamp_box_padding as f32;
    let (box_w, box_h) = ((right - left) + 2.0 * pad, (bottom - top) + 2.0 * pad);
    let m = layout.timestamp_margin;
    let (x0, y0) = (rect.x as f32, rect.y as f32);
    let near_x = (rect.x + m) as f32;
    let far_x = (rect.x + rect.w - m) as f32 - box_w;
    let near_y = (rect.y + m) as f32;
    let far_y = (rect.y + rect.h - m) as f32 - box_h;
    let (box_left, box_top) = match position {
        TimestampPosition::BottomRight => (far_x, far_y),
        TimestampPosition::BottomLeft => (near_x, far_y),
        TimestampPosition::TopRight => (far_x, near_y),
        TimestampPosition::TopLeft => (near_x, near_y),
        TimestampPosition::Center => (
            x0 + ((rect.w as f32 - box_w) / 2.0).floor(),
            y0 + ((rect.h as f32 - box_h) / 2.0).floor(),
        ),
    };
    let (box_right, box_bottom) = (box_left + box_w, box_top + box_h);
    darken(
        image,
        box_left as i64,
        box_top as i64,
        box_right as i64,
        box_bottom as i64,
        TIMESTAMP_BOX_ALPHA,
    );
    let origin_x = box_left + pad - left;
    let baseline = box_top + pad - top + ascent;
    font.draw(image, origin_x, baseline, text, TIMESTAMP_TEXT);
}

/// 헤더 + 격자 + 타임스탬프를 합성한다.
///
/// frames가 칸보다 적으면 남는 칸은 비워 둔다(매우 짧은 영상).
/// image가 None인 프레임은 실패 칸(회색)으로 그린다.
pub fn render_sheet(
    layout: &Layout,
    header_lines: &[String],
    frames: &[Frame],
    fonts: &FontFiles,
    theme_name: ThemeName,
    timestamp: Option<TimestampPosition>, // None이면 그리지 않음
) -> RgbImage {
    let colors = theme(theme_name);
    let mut sheet = RgbImage::from_pixel(
        layout.width as u32,
        layout.height as u32,
        image::Rgb(colors.background),
    );

    if !header_lines.is_empty() {
        let size = layout.header_font_size as f32;
        let (bold, regular) = (
            Text::new(&fonts.bold, size),
            Text::new(&fonts.regular, size),
        );
        let max_width = (layout.width - 2 * layout.padding) as f32;
        for (i, line) in header_lines.iter().enumerate() {
            let font = if i == 0 { &bold } else { &regular }; // 첫 줄만 굵게
            // 줄 상자의 세로 가운데 (Pillow anchor "lm": 어센더와 디센더의 중간)
            let middle = (layout.header_top + i as i64 * layout.header_line_height) as f32
                + layout.header_line_height as f32 / 2.0;
            let baseline = middle + (font.ascent() + font.descent()) / 2.0;
            let text = fit_text(line, font, max_width);
            font.draw(
                &mut sheet,
                layout.padding as f32,
                baseline,
                &text,
                colors.header_text,
            );
        }
    }

    let ts_font = Text::new(&fonts.regular, layout.timestamp_font_size as f32);
    for (rect, frame) in layout.cells.iter().zip(frames) {
        let (x1, y1) = (rect.x + rect.w, rect.y + rect.h);
        match &frame.image {
            None => fill(&mut sheet, rect.x, rect.y, x1, y1, colors.failed_cell),
            Some(image) => {
                fill(&mut sheet, rect.x, rect.y, x1, y1, colors.cell_background);
                let resized;
                let image = if image.dimensions() != (rect.w as u32, rect.h as u32) {
                    resized = image::imageops::resize(
                        image,
                        rect.w as u32,
                        rect.h as u32,
                        image::imageops::FilterType::CatmullRom,
                    );
                    &resized
                } else {
                    image
                };
                image::imageops::replace(&mut sheet, image, rect.x, rect.y);
            }
        }
        if let Some(position) = timestamp {
            draw_timestamp(
                &mut sheet,
                *rect,
                &format_duration(frame.time),
                &ts_font,
                layout,
                position,
            );
        }
    }
    sheet
}

fn encode(image: &RgbImage, format: OutputFormat, quality: i64) -> std::io::Result<Vec<u8>> {
    let (w, h) = image.dimensions();
    let mut out = Vec::new();
    match format {
        OutputFormat::Webp => {
            // encode()는 실패하면 panic하므로 에러를 돌려주는 encode_simple을 쓴다
            let encoded = webp::Encoder::from_rgb(image.as_raw(), w, h)
                .encode_simple(false, quality as f32)
                .map_err(|e| std::io::Error::other(format!("WebP 인코딩 실패: {e:?}")))?;
            out.extend_from_slice(&encoded);
        }
        OutputFormat::Jpg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut out,
                quality.clamp(1, 100) as u8,
            )
            .write_image(image.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .map_err(std::io::Error::other)?;
        }
        OutputFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(image.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                .map_err(std::io::Error::other)?;
        }
    }
    Ok(out)
}

/// 임시 이름(`.{이름}.tmp`)으로 쓴 뒤 바꾼다. 중간에 끊겨도 깨진 파일이 남지 않는다.
pub fn save_sheet(
    image: &RgbImage,
    path: &Path,
    format: OutputFormat,
    quality: i64,
) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let result = (|| {
        let bytes = encode(image, format, quality)?;
        SAVING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tmp.clone());
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    SAVING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| *p != tmp);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::find_fonts;
    use crate::layout::{LayoutInput, compute_layout};

    fn layout(header_lines: i64) -> Layout {
        compute_layout(
            &LayoutInput {
                width: 640,
                columns: 2,
                rows: 2,
                gap: 8,
                aspect: 16.0 / 9.0,
                header_lines,
            },
            None,
        )
        .unwrap()
    }

    fn frames(l: &Layout, count: usize, color: Rgb) -> Vec<Frame> {
        (0..count)
            .map(|i| Frame {
                time: 60.0 * (i + 1) as f64,
                image: Some(RgbImage::from_pixel(
                    l.cell_width as u32,
                    l.cell_height as u32,
                    image::Rgb(color),
                )),
            })
            .collect()
    }

    #[test]
    fn background_frames_failed_and_empty_cells() {
        let fonts = find_fonts("");
        let l = layout(0);
        let mut fs = frames(&l, 2, [10, 200, 30]);
        fs[1].image = None;
        let sheet = render_sheet(&l, &[], &fs, &fonts, ThemeName::Dark, None);
        assert_eq!(sheet.dimensions(), (l.width as u32, l.height as u32));
        assert_eq!(sheet.get_pixel(1, 1).0, theme(ThemeName::Dark).background);
        let (c0, c1, c2) = (l.cells[0], l.cells[1], l.cells[2]);
        assert_eq!(sheet.get_pixel(c0.x as u32, c0.y as u32).0, [10, 200, 30]);
        assert_eq!(
            sheet.get_pixel(c1.x as u32 + 5, c1.y as u32 + 5).0,
            theme(ThemeName::Dark).failed_cell
        );
        assert_eq!(
            sheet.get_pixel(c2.x as u32 + 5, c2.y as u32 + 5).0,
            theme(ThemeName::Dark).background
        );
    }

    #[test]
    fn timestamp_box_bottom_right() {
        let fonts = find_fonts("");
        let l = layout(0);
        let sheet = render_sheet(
            &l,
            &[],
            &frames(&l, 4, [255, 255, 255]),
            &fonts,
            ThemeName::Light,
            Some(TimestampPosition::BottomRight),
        );
        let r = l.cells[0];
        let m = l.timestamp_margin;
        let inside = sheet
            .get_pixel((r.x + r.w - m - 1) as u32, (r.y + r.h - m - 1) as u32)
            .0;
        assert!(
            inside.iter().all(|&c| (c as i32 - 102).abs() <= 2),
            "{inside:?}"
        );
        assert_eq!(
            sheet
                .get_pixel((r.x + r.w - 1) as u32, (r.y + r.h - 1) as u32)
                .0,
            [255, 255, 255]
        );
        assert_eq!(
            sheet.get_pixel((r.x + 2) as u32, (r.y + 2) as u32).0,
            [255, 255, 255]
        );
    }

    #[test]
    fn timestamp_positions() {
        let fonts = find_fonts("");
        let l = layout(0);
        let r = l.cells[0];
        let m = l.timestamp_margin;
        let (left, right) = (r.x + m, r.x + r.w - m - 1);
        let (top, bottom) = (r.y + m, r.y + r.h - m - 1);
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        let dim = |p: [u8; 3]| p.iter().all(|&c| (c as i32 - 102).abs() <= 2);
        // (위치, 박스 안이어야 할 점, 박스 밖이어야 할 점들)
        let cases = [
            (
                TimestampPosition::BottomLeft,
                (left, bottom),
                [(right, bottom), (left, top)],
            ),
            (
                TimestampPosition::TopRight,
                (right, top),
                [(right, bottom), (left, top)],
            ),
            (
                TimestampPosition::TopLeft,
                (left, top),
                [(right, top), (left, bottom)],
            ),
            (
                TimestampPosition::Center,
                (cx, cy),
                [(left, top), (right, bottom)],
            ),
        ];
        for (position, inside, outside) in cases {
            let sheet = render_sheet(
                &l,
                &[],
                &frames(&l, 4, [255, 255, 255]),
                &fonts,
                ThemeName::Light,
                Some(position),
            );
            let px = |(x, y): (i64, i64)| sheet.get_pixel(x as u32, y as u32).0;
            assert!(dim(px(inside)), "{position:?} 안쪽 {:?}", px(inside));
            for p in outside {
                assert_eq!(px(p), [255, 255, 255], "{position:?} 바깥 {p:?}");
            }
        }
    }

    #[test]
    fn header_drawn_in_header_area() {
        let fonts = find_fonts("");
        let l = layout(2);
        let lines = vec!["파일명: 한글.mp4".to_string(), "크기: 1.00MB".to_string()];
        let sheet = render_sheet(
            &l,
            &lines,
            &[],
            &fonts,
            ThemeName::Light,
            Some(TimestampPosition::BottomRight),
        );
        let dark = (0..l.cells[0].y as u32)
            .flat_map(|y| (0..l.width as u32).map(move |x| (x, y)))
            .filter(|&(x, y)| sheet.get_pixel(x, y).0[0] < 50)
            .count();
        assert!(dark > 50, "헤더 글자 픽셀 {dark}");
    }

    #[test]
    fn fit_text_middle_ellipsis() {
        let fonts = find_fonts("");
        let font = Text::new(&fonts.regular, 14.0);
        assert_eq!(
            fit_text("File Name: a.mp4", &font, 1000.0),
            "File Name: a.mp4"
        );
        let long = format!("File Name: {}_끝.mp4", "아주긴파일이름".repeat(20));
        let fitted = fit_text(&long, &font, 300.0);
        assert!(fitted.contains('…'));
        assert!(font.length(&fitted) <= 300.0);
        assert!(fitted.starts_with("File Name: ") && fitted.ends_with("_끝.mp4"));
    }

    #[test]
    fn save_formats_and_no_temp_left() {
        let dir = tempfile::tempdir().unwrap();
        let image = RgbImage::from_pixel(64, 32, image::Rgb([200, 100, 50]));
        for (format, magic) in [
            (OutputFormat::Webp, &b"RIFF"[..]),
            (OutputFormat::Jpg, &[0xFF, 0xD8][..]),
            (OutputFormat::Png, &[0x89, b'P', b'N', b'G'][..]),
        ] {
            let out = dir
                .path()
                .join(format!("영상 이름_sheet.{}", format.extension()));
            save_sheet(&image, &out, format, 85).unwrap();
            let bytes = std::fs::read(&out).unwrap();
            assert!(bytes.starts_with(magic));
            let decoded = image::load_from_memory(&bytes);
            if let Ok(d) = decoded {
                assert_eq!((d.width(), d.height()), (64, 32));
            }
        }
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }
}
