//! exe 아이콘과 파일 정보를 넣는다.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 정적 FFmpeg(vcpkg)가 쓰는 Windows 라이브러리 중 ffmpeg-sys-next가 빠뜨리는 것:
        // TLS(schannel) → crypt32·ncrypt, Media Foundation → mfuuid·strmiids
        for lib in ["crypt32", "ncrypt", "mfuuid", "strmiids"] {
            println!("cargo:rustc-link-lib={lib}");
        }
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "VidGlance");
        res.set("ProductName", "VidGlance");
        res.compile().expect("아이콘 리소스를 넣지 못했습니다");
    }
}
