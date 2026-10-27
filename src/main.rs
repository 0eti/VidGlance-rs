//! VidGlance: 영상을 끌어다 놓으면 정보 헤더 + 프레임 격자 시트를 만든다.

mod app;
mod batch;
mod config;
mod extract;
mod fonts;
mod formatting;
mod layout;
mod probe;
mod render;
#[cfg(test)]
mod test_media;

fn main() {
    std::process::exit(app::main());
}
