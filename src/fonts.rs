//! 폰트 탐색 (spec §8): 설정 → 맑은 고딕 → 내장 Pretendard(exe 안).

use ab_glyph::FontArc;
use std::path::{Path, PathBuf};

static PRETENDARD_REGULAR: &[u8] = include_bytes!("../assets/fonts/Pretendard-Regular.otf");
static PRETENDARD_BOLD: &[u8] = include_bytes!("../assets/fonts/Pretendard-Bold.otf");

pub struct FontFiles {
    pub regular: FontArc,
    pub bold: FontArc,
    pub source: String, // 어디서 찾았는지 (-v 출력용)
    pub warnings: Vec<String>,
}

pub fn windows_fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("Fonts")
}

fn load(path: &Path) -> Option<FontArc> {
    FontArc::try_from_vec(std::fs::read(path).ok()?).ok()
}

/// 보통·굵게 폰트를 찾는다. 내장 Pretendard가 있으므로 항상 성공한다.
pub fn find_fonts(custom: &str) -> FontFiles {
    find_fonts_in(custom, &windows_fonts_dir())
}

pub fn find_fonts_in(custom: &str, windows_fonts: &Path) -> FontFiles {
    let mut warnings = Vec::new();
    if !custom.is_empty() {
        let path = PathBuf::from(custom);
        if let Some(font) = load(&path) {
            return FontFiles {
                regular: font.clone(),
                bold: font,
                source: format!("설정 font ({})", path.display()),
                warnings,
            };
        }
        warnings.push(format!(
            "설정한 폰트 파일이 없거나 읽을 수 없어 기본 폰트를 씁니다: {}",
            path.display()
        ));
    }

    let malgun = windows_fonts.join("malgun.ttf");
    if let Some(regular) = load(&malgun) {
        let bold_path = windows_fonts.join("malgunbd.ttf");
        let bold = load(&bold_path).unwrap_or_else(|| regular.clone());
        return FontFiles {
            regular,
            bold,
            source: format!("맑은 고딕 ({})", malgun.display()),
            warnings,
        };
    }

    FontFiles {
        regular: FontArc::try_from_slice(PRETENDARD_REGULAR).expect("내장 Pretendard Regular"),
        bold: FontArc::try_from_slice(PRETENDARD_BOLD).expect("내장 Pretendard Bold"),
        source: "내장 Pretendard (exe 안)".into(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::Font;

    #[test]
    fn malgun_default_when_present() {
        let dir = windows_fonts_dir();
        if dir.join("malgun.ttf").is_file() {
            assert!(find_fonts("").source.starts_with("맑은 고딕"));
        }
    }

    #[test]
    fn bundled_pretendard_when_no_malgun() {
        let empty = tempfile::tempdir().unwrap();
        let f = find_fonts_in("", empty.path());
        assert!(f.source.starts_with("내장 Pretendard"));
        // 한글 글리프가 실제로 있다 (□ 대체 글리프가 아님)
        assert_ne!(f.regular.glyph_id('가').0, 0);
        assert_ne!(f.bold.glyph_id('한').0, 0);
    }

    #[test]
    fn missing_custom_falls_back_with_warning() {
        let empty = tempfile::tempdir().unwrap();
        let f = find_fonts_in("C:/없는/폰트.ttf", empty.path());
        assert!(f.source.starts_with("내장 Pretendard"));
        assert!(f.warnings[0].contains("폰트.ttf"));
    }
}
