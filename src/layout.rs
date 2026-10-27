//! 순수 함수: 시트 좌표 계산 (spec §8). I/O를 하지 않는다.
//!
//! 반올림은 Python `round()`와 같은 짝수 반올림(`round_ties_even`)을 쓴다.
//! 그래야 Python 버전과 같은 크기·좌표가 나온다.

pub const PADDING: i64 = 10; // 바깥 여백 (헤더 아래 여백도 같은 값)
const LINE_SPACING: f64 = 1.3; // 헤더 줄 높이 = 글자 크기 × 1.3
const HEADER_FONT_RATIO: f64 = 0.014; // 헤더 글자 = 시트 너비 × 0.014
const TIMESTAMP_FONT_RATIO: f64 = 0.06; // 타임스탬프 글자 = 칸 너비 × 0.06
const TIMESTAMP_MARGIN_RATIO: f64 = 0.02; // 박스와 칸 가장자리 사이 = 칸 너비 × 0.02
const TIMESTAMP_BOX_PADDING_RATIO: f64 = 0.25; // 박스 안쪽 여백 = 글자 크기 × 0.25
const MIN_FONT_SIZE: i64 = 11;
const MIN_TIMESTAMP_MARGIN: i64 = 3;
const MIN_CELL_WIDTH: i64 = 16;
pub const WEBP_MAX_SIZE: i64 = 16383;

/// Python `round()`와 같은 반올림 (.5는 짝수 쪽)
fn py_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: i64,
    pub height: i64,
    pub padding: i64,
    pub header_font_size: i64,
    pub header_line_height: i64,
    pub header_top: i64, // 첫 헤더 줄의 위쪽 y
    pub cell_width: i64,
    pub cell_height: i64,
    pub cells: Vec<Rect>, // 왼쪽 위부터 행 순서
    pub timestamp_font_size: i64,
    pub timestamp_margin: i64,
    pub timestamp_box_padding: i64,
    pub shrunk_from: Option<i64>, // 최대 높이 때문에 너비를 줄였으면 원래 너비
}

#[derive(Debug, PartialEq)]
pub struct LayoutError(pub String);

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LayoutError {}

#[derive(Debug, Clone, Copy)]
pub struct LayoutInput {
    pub width: i64,
    pub columns: i64,
    pub rows: i64,
    pub gap: i64,
    pub aspect: f64, // 표시 비율 (가로/세로)
    pub header_lines: i64,
}

fn layout_at(width: i64, inp: &LayoutInput) -> Result<Layout, LayoutError> {
    let (columns, rows, gap) = (inp.columns, inp.rows, inp.gap);
    let cell_w = (width - 2 * PADDING - (columns - 1) * gap).div_euclid(columns);
    if cell_w < MIN_CELL_WIDTH {
        return Err(LayoutError(format!(
            "칸 너비가 {cell_w}px로 너무 작습니다. width를 늘리거나 columns·gap을 줄이세요"
        )));
    }
    let cell_h = py_round(cell_w as f64 / inp.aspect).max(1);

    let header_font = py_round(width as f64 * HEADER_FONT_RATIO).max(MIN_FONT_SIZE);
    let line_h = py_round(header_font as f64 * LINE_SPACING);
    let header_h = if inp.header_lines > 0 {
        inp.header_lines * line_h + PADDING
    } else {
        0
    };

    let grid_w = columns * cell_w + (columns - 1) * gap;
    let left = (width - grid_w).div_euclid(2); // 내림으로 남은 px를 양쪽에 나눈다
    let top = PADDING + header_h;
    let cells = (0..rows)
        .flat_map(|r| {
            (0..columns).map(move |c| Rect {
                x: left + c * (cell_w + gap),
                y: top + r * (cell_h + gap),
                w: cell_w,
                h: cell_h,
            })
        })
        .collect();
    let height = top + rows * cell_h + (rows - 1) * gap + PADDING;
    let ts_font = py_round(cell_w as f64 * TIMESTAMP_FONT_RATIO).max(MIN_FONT_SIZE);

    Ok(Layout {
        width,
        height,
        padding: PADDING,
        header_font_size: header_font,
        header_line_height: line_h,
        header_top: PADDING,
        cell_width: cell_w,
        cell_height: cell_h,
        cells,
        timestamp_font_size: ts_font,
        timestamp_margin: py_round(cell_w as f64 * TIMESTAMP_MARGIN_RATIO)
            .max(MIN_TIMESTAMP_MARGIN),
        timestamp_box_padding: py_round(ts_font as f64 * TIMESTAMP_BOX_PADDING_RATIO),
        shrunk_from: None,
    })
}

/// 설정·영상 비율·헤더 줄 수 → 시트 크기와 칸 좌표.
///
/// max_height(예: WebP 16383)를 넘으면 시트 너비를 비율대로 줄여 다시 계산한다.
pub fn compute_layout(inp: &LayoutInput, max_height: Option<i64>) -> Result<Layout, LayoutError> {
    let mut layout = layout_at(inp.width, inp)?;
    let Some(max_height) = max_height else {
        return Ok(layout);
    };
    if layout.height <= max_height {
        return Ok(layout);
    }
    let mut reduced = inp.width;
    while layout.height > max_height {
        // 여백·간격은 고정이라 한 번에 딱 맞지 않을 수 있어 넘지 않을 때까지 줄인다
        let scaled = (reduced as f64 * max_height as f64 / layout.height as f64).floor() as i64;
        reduced = (reduced - 1).min(scaled);
        layout = layout_at(reduced, inp)?;
    }
    layout.shrunk_from = Some(inp.width);
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(
        f: impl FnOnce(&mut LayoutInput),
        max_height: Option<i64>,
    ) -> Result<Layout, LayoutError> {
        let mut inp = LayoutInput {
            width: 1024,
            columns: 4,
            rows: 5,
            gap: 8,
            aspect: 16.0 / 9.0,
            header_lines: 5,
        };
        f(&mut inp);
        compute_layout(&inp, max_height)
    }

    fn default() -> Layout {
        lay(|_| {}, None).unwrap()
    }

    #[test]
    fn default_sheet() {
        let l = default();
        assert_eq!((l.cell_width, l.cell_height), (245, 138));
        assert_eq!((l.header_font_size, l.header_line_height), (14, 18));
        assert_eq!(
            l.cells[0],
            Rect {
                x: 10,
                y: 110,
                w: 245,
                h: 138
            }
        );
        assert_eq!(
            l.cells[1],
            Rect {
                x: 10 + 245 + 8,
                y: 110,
                w: 245,
                h: 138
            }
        );
        assert_eq!(
            l.cells[4],
            Rect {
                x: 10,
                y: 110 + 138 + 8,
                w: 245,
                h: 138
            }
        );
        assert_eq!(l.cells.len(), 20);
        assert_eq!(l.height, 10 + 100 + 5 * 138 + 4 * 8 + 10);
        assert_eq!(l.width, 1024);
        assert_eq!((l.timestamp_font_size, l.timestamp_margin), (15, 5));
        assert_eq!(l.timestamp_box_padding, 4);
        assert_eq!(l.shrunk_from, None);
    }

    #[test]
    fn grid_fits_inside_padding() {
        let l = default();
        let last = l.cells.last().unwrap();
        assert!(last.x + last.w <= l.width - l.padding);
        assert_eq!(last.y + last.h, l.height - l.padding);
    }

    #[test]
    fn leftover_pixels_are_split() {
        let l = lay(|i| i.width = 1027, None).unwrap();
        assert_eq!(l.cells[0].x, 11);
        assert_eq!(l.width - (l.cells[3].x + l.cells[3].w), 12);
    }

    #[test]
    fn gap_zero_cells_touch() {
        let l = lay(|i| i.gap = 0, None).unwrap();
        let (a, b, below) = (l.cells[0], l.cells[1], l.cells[4]);
        assert_eq!(a.x + a.w, b.x);
        assert_eq!(a.y + a.h, below.y);
    }

    #[test]
    fn no_header_and_line_count() {
        let l = lay(|i| i.header_lines = 0, None).unwrap();
        assert_eq!(l.cells[0].y, l.padding);
        assert_eq!(default().height - l.height, 100);
        let two = lay(|i| i.header_lines = 2, None).unwrap();
        assert_eq!(two.cells[0].y, 10 + 2 * 18 + 10);
    }

    #[test]
    fn vertical_and_anamorphic() {
        let v = lay(|i| i.aspect = 9.0 / 16.0, None).unwrap();
        assert_eq!(v.cell_width, 245);
        assert_eq!(
            v.cell_height,
            (245.0f64 * 16.0 / 9.0).round_ties_even() as i64
        );
    }

    #[test]
    fn min_font_sizes() {
        let l = lay(
            |i| {
                i.width = 320;
                i.columns = 10;
                i.gap = 0;
            },
            None,
        )
        .unwrap();
        assert_eq!(l.header_font_size, 11);
        assert_eq!(l.timestamp_font_size, 11);
        assert_eq!(l.timestamp_margin, 3);
    }

    #[test]
    fn cell_too_small() {
        let err = lay(
            |i| {
                i.width = 320;
                i.columns = 20;
                i.gap = 100;
            },
            None,
        )
        .unwrap_err();
        assert!(err.0.contains("너무 작습니다"));
    }

    #[test]
    fn webp_limit_shrinks_width() {
        let setup = |i: &mut LayoutInput| {
            i.width = 2000;
            i.rows = 20;
            i.aspect = 9.0 / 16.0;
        };
        assert!(lay(setup, None).unwrap().height > WEBP_MAX_SIZE);
        let l = lay(setup, Some(WEBP_MAX_SIZE)).unwrap();
        assert!(l.height <= WEBP_MAX_SIZE);
        assert!(l.height as f64 > WEBP_MAX_SIZE as f64 * 0.98);
        assert!(l.width < 2000);
        assert_eq!(l.shrunk_from, Some(2000));
        assert_eq!(l.cells.len(), 80);
    }

    #[test]
    fn within_limit_is_unchanged() {
        assert_eq!(lay(|_| {}, Some(WEBP_MAX_SIZE)).unwrap(), default());
    }

    #[test]
    fn python_round_is_ties_even() {
        assert_eq!(py_round(14.5), 14);
        assert_eq!(py_round(15.5), 16);
        assert_eq!(py_round(4.25 * 1.0), 4);
    }
}
