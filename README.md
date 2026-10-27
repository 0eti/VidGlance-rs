# VidGlance (Rust)

영상 파일을 `VidGlance.exe` 위로 끌어다 놓으면, 같은 폴더에 **한 장짜리 썸네일 시트**(`영상이름_sheet.webp`)를 만드는 Windows 도구입니다. [Python 버전 VidGlance](https://github.com/0eti/VidGlance)를 Rust로 다시 만든 것으로, 기능과 시트 모양은 같습니다.

- 위쪽에 영상 정보: 파일명, 크기, 해상도(비율), fps, 비디오·오디오 코덱, 재생 시간
- 아래에 4×5 격자 프레임과 칸마다 타임스탬프
- 여러 영상과 폴더를 한꺼번에 처리하고, 한글 파일명·경로를 지원합니다

## Python 버전과 다른 점

| | Python 0.1.1 | Rust 0.2.0 |
|---|---|---|
| 배포 | onefile 82MB 또는 onedir 폴더 210MB | **exe 하나 24MB** (FFmpeg·dav1d·폰트 포함, 다른 DLL 필요 없음) |
| 시작 | onefile 약 4~5초, onedir 0.3초 | **0.02초** |
| 4K AV1 영상 1개 | onedir 약 1.1초 | **약 0.3초** |
| 4K AV1 영상 5개 한꺼번에 | onedir 약 5.4초 | **약 1.2초** (3개씩 동시에 처리) |
| 1080p H.264 60초 (키프레임 8초 간격) | onedir 약 2.6초 | **약 1.3초** |
| 9.6GB fragmented MP4 (3시간 27분) | onedir 약 4.1초 | **약 2.1초** |
| 설정 파일 위치 | exe 옆 `config.json` | **항상 `%APPDATA%\VidGlance\config.json`** (exe 옆에는 만들지 않음) |
| 영상 정보 | ffprobe.exe 실행 | exe 안의 FFmpeg 라이브러리로 직접 읽음 (`ffmpeg_dir` 설정·`--ffmpeg-dir` 없음) |

Python 버전의 `config.json`을 `%APPDATA%\VidGlance\`에 복사해도 그대로 쓸 수 있습니다(`ffmpeg_dir`은 경고 없이 무시).

## 사용법

1. `VidGlance.exe`를 원하는 폴더에 둡니다.
2. 영상 파일(여러 개 가능)이나 폴더를 **`VidGlance.exe` 아이콘 위로 끌어다 놓습니다.**
3. 각 영상과 같은 폴더에 `영상이름_sheet.webp`가 생깁니다. 모두 성공하면 3초 뒤 창이 저절로 닫히고, 실패가 있으면 이유를 보여 준 뒤 키 입력을 기다립니다.

- 여러 영상은 3개씩 동시에 처리하고, 결과는 놓은 순서대로 보여 줍니다.
- 이미 시트가 있는 영상은 건너뜁니다(`overwrite`로 바꿀 수 있음).
- 폴더를 놓으면 그 안의 영상(`mp4 mkv avi mov wmv flv webm m4v ts m2ts mpg mpeg 3gp`)만 처리합니다. 하위 폴더는 `recursive`가 `true`일 때만.
- exe를 **더블클릭**하면 설정 파일 위치를 알려 줍니다(없으면 만듭니다).

> 서명되지 않은 exe라서 처음 실행할 때 Windows SmartScreen 창이 뜰 수 있습니다. **추가 정보 → 실행**을 누르면 됩니다.

## 설정 (`%APPDATA%\VidGlance\config.json`)

탐색기 주소창에 `%APPDATA%\VidGlance`를 입력하면 폴더가 열립니다. 메모장으로 고친 뒤 저장하면 다음 실행부터 반영됩니다. 다른 설정 파일을 쓰려면 `--config 경로`를 붙입니다.

```json
{
  "config_version": 1,
  "columns": 4,
  "rows": 5,
  "width": 1024,
  "gap": 8,
  "theme": "light",
  "language": "en",
  "header": {
    "file_name": true,
    "size": true,
    "resolution": true,
    "fps": true,
    "video_codec": true,
    "audio_codec": true,
    "duration": true
  },
  "format": "webp",
  "quality": 85,
  "suffix": "_sheet",
  "output_dir": "",
  "overwrite": false,
  "show_timestamp": true,
  "recursive": false,
  "skip_start_seconds": 0,
  "skip_end_seconds": 0,
  "seek_mode": "fast",
  "font": "",
  "close_delay_seconds": 3
}
```

| 키 | 뜻 | 허용 값 |
|---|---|---|
| `columns`, `rows` | 격자 열·행 수 | 1–20 |
| `width` | 시트 가로 크기(px) | 320–8000 |
| `gap` | 칸 사이 간격(px) | 0–100 |
| `theme` | 밝은 배경 / 어두운 배경 | `"light"` / `"dark"` |
| `language` | 헤더 레이블 언어 | `"en"` / `"ko"` |
| `header.*` | 헤더 항목별 표시 여부. 모두 `false`면 헤더 없이 격자만 | `true` / `false` |
| `format` | 저장 형식 | `"webp"` / `"jpg"` / `"png"` |
| `quality` | 압축 품질 (png는 무시) | 1–100 |
| `suffix` | 출력 파일명 꼬리 | 비어 있지 않은 문자열. `< > : " / \ \| ? *` 금지 |
| `output_dir` | 저장 폴더. 비우면 영상 폴더, 상대 경로면 각 영상 폴더 기준 | 경로 |
| `overwrite` | 이미 있는 시트를 덮어쓸지 | `true` / `false` |
| `show_timestamp` | 칸마다 타임스탬프를 그릴지 | `true` / `false` |
| `recursive` | 폴더를 놓았을 때 하위 폴더까지 | `true` / `false` |
| `skip_start_seconds`, `skip_end_seconds` | 앞·뒤에서 뺄 구간(초) | 0 이상 |
| `seek_mode` | `fast`는 균등 시점에서 가장 가까운 키프레임(빠름, 오차 보통 1~3초), `accurate`는 정확한 시점(느림). 타임스탬프는 항상 실제 장면 시각 | `"fast"` / `"accurate"` |
| `font` | 글꼴 파일 경로. 비우면 맑은 고딕 → 내장 Pretendard | 경로 |
| `close_delay_seconds` | 모두 성공했을 때 창이 닫히기까지 초. 0이면 바로, 음수면 항상 키 대기 | 정수 |

JSON에서 Windows 경로의 `\`는 `\\`로 두 번 쓰거나 `/`를 씁니다: `"output_dir": "D:/썸네일"`.

## 명령줄 옵션

지정한 옵션은 이번 실행에서만 설정을 덮어씁니다. 명령줄에서 실행하면 끝나고 창이 기다리지 않습니다.

```
VidGlance.exe [옵션] <파일 또는 폴더...>

  -g, --grid CxR         격자 (예: 5x6)
  -w, --width PX
      --gap PX
      --theme THEME      light | dark
      --lang LANG        en | ko
  -o, --output-dir DIR
  -r, --recursive
      --format FMT       webp | jpg | png
      --quality N
      --overwrite
      --no-timestamp
      --font PATH
      --seek MODE        fast | accurate
      --config PATH      설정 파일 직접 지정
      --no-pause         끝나고 기다리지 않음
  -q, --quiet            요약만 출력
  -v, --verbose          FFmpeg 버전·폰트·단계별 시간 출력
  -V, --version
```

종료 코드: `0` 모두 성공 또는 건너뜀, `1` 하나 이상 실패, `2` 설정 오류·잘못된 옵션.

## 빌드

필요한 것: Rust(MSVC), Visual Studio Build Tools(C++), LLVM(libclang), [vcpkg](https://github.com/microsoft/vcpkg).

```
# FFmpeg + dav1d 정적 빌드 (처음 한 번, 약 30분)
vcpkg install "ffmpeg[core,avcodec,avformat,swscale,dav1d,zlib]:x64-windows-static-release" --overlay-triplets=vcpkg-triplets

cargo build --release      # → target/release/VidGlance.exe
cargo test                 # 테스트 영상: ../VidGlance/tests/_media (또는 환경 변수 VIDGLANCE_MEDIA)
```

vcpkg 위치는 `.cargo/config.toml`의 `VCPKG_ROOT`(기본 `D:\Dev\Tools\vcpkg`)이고, 환경 변수로 바꿀 수 있습니다. VC++ 런타임도 정적으로 넣어(`+crt-static`) exe 하나로 동작합니다.

## 라이선스와 고지

VidGlance 자체의 라이선스는 아직 정하지 않았습니다.

exe에는 아래 라이브러리가 정적으로 들어 있습니다.

| 구성 요소 | 버전 | 라이선스 | 출처 |
|---|---|---|---|
| FFmpeg (libavcodec, libavformat, libavutil, libswscale) | 9.0.2 | **LGPL v2.1 이상** (GPL 기능 없이 빌드) | <https://ffmpeg.org/download.html> |
| dav1d (AV1 디코더) | 1.5.4 | BSD-2-Clause | <https://code.videolan.org/videolan/dav1d> |
| zlib | 1.3.2 | zlib License | <https://zlib.net> |
| libwebp (libwebp-sys) | | BSD-3-Clause | <https://chromium.googlesource.com/webm/libwebp> |
| Pretendard 글꼴 | 1.3.9 | SIL Open Font License 1.1 (`assets/fonts/Pretendard-LICENSE.txt`) | <https://github.com/orioncactus/pretendard> |
| Rust 크레이트 (ffmpeg-next, image, ab_glyph, clap, serde_json 등) | | 대부분 MIT 또는 Apache-2.0 | `Cargo.lock` |

FFmpeg를 정적으로 링크했으므로 LGPL에 따라 이 저장소의 소스와 빌드 방법(위 "빌드")으로 다른 FFmpeg 빌드와 다시 링크할 수 있습니다. FFmpeg는 FFmpeg 프로젝트를 시작한 Fabrice Bellard의 상표이며, VidGlance는 FFmpeg 프로젝트와 관계가 없습니다.
