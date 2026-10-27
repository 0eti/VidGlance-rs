//! 테스트 영상 위치: Python 버전이 만든 `../VidGlance/tests/_media`를 그대로 쓴다.
//! 없으면 Python 버전 폴더에서 `uv run pytest`를 한 번 실행하면 생긴다.
//! 다른 곳에 있으면 환경 변수 VIDGLANCE_MEDIA로 지정한다.

use std::path::PathBuf;

pub fn media_dir() -> PathBuf {
    std::env::var_os("VIDGLANCE_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("VidGlance")
                .join("tests")
                .join("_media")
        })
}

pub fn media(name: &str) -> PathBuf {
    let path = media_dir().join(name);
    assert!(
        path.is_file(),
        "테스트 영상이 없습니다: {} (../VidGlance에서 uv run pytest)",
        path.display()
    );
    path
}
