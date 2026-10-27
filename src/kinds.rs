//! File kinds and the extension <-> MIME tables. Pure functions, unit-tested.
//!
//! Used for three decisions:
//!   * which extension an original file really has (also `tar.gz`, unknown formats, nameless voice notes ...),
//!   * which MIME type the renamed copy gets (so a video stays a video),
//!   * whether a thumbnail makes sense for the file (video, audio, image, pdf - not txt/zip/apk ...).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Image,
    Pdf,
    Other,
}

impl Kind {
    /// Telegram shows a custom cover for these. For everything else (txt, zip, apk, docx ...) it is ignored,
    /// so none is attached.
    pub fn takes_thumbnail(self) -> bool {
        !matches!(self, Kind::Other)
    }
}

/// (extension, MIME type). The first row of a MIME type is the extension chosen for it.
const TABLE: &[(&str, &str)] = &[
    // video
    ("mp4", "video/mp4"),
    ("m4v", "video/x-m4v"),
    ("mkv", "video/x-matroska"),
    ("mkv", "video/matroska"),
    ("webm", "video/webm"),
    ("mov", "video/quicktime"),
    ("avi", "video/x-msvideo"),
    ("avi", "video/avi"),
    ("avi", "video/msvideo"),
    ("wmv", "video/x-ms-wmv"),
    ("asf", "video/x-ms-asf"),
    ("flv", "video/x-flv"),
    ("f4v", "video/x-f4v"),
    ("mpg", "video/mpeg"),
    ("mpeg", "video/mpeg"),
    ("vob", "video/mpeg"),
    ("3gp", "video/3gpp"),
    ("3g2", "video/3gpp2"),
    ("ogv", "video/ogg"),
    ("ts", "video/mp2t"),
    ("m2ts", "video/mp2t"),
    ("mts", "video/mp2t"),
    ("divx", "video/divx"),
    ("rmvb", "video/vnd.rn-realvideo"),
    // audio
    ("mp3", "audio/mpeg"),
    ("mp3", "audio/mp3"),
    ("m4a", "audio/mp4"),
    ("m4a", "audio/x-m4a"),
    ("aac", "audio/aac"),
    ("flac", "audio/flac"),
    ("flac", "audio/x-flac"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/opus"),
    ("wav", "audio/x-wav"),
    ("wav", "audio/wav"),
    ("wma", "audio/x-ms-wma"),
    ("mka", "audio/x-matroska"),
    ("amr", "audio/amr"),
    ("ac3", "audio/ac3"),
    ("aiff", "audio/aiff"),
    ("mid", "audio/midi"),
    // images
    ("jpg", "image/jpeg"),
    ("jpg", "image/jpg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("svg", "image/svg+xml"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("avif", "image/avif"),
    ("ico", "image/x-icon"),
    ("psd", "image/vnd.adobe.photoshop"),
    ("djvu", "image/vnd.djvu"),
    // documents
    ("pdf", "application/pdf"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("rtf", "application/rtf"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("epub", "application/epub+zip"),
    ("mobi", "application/x-mobipocket-ebook"),
    ("cbz", "application/vnd.comicbook+zip"),
    ("cbr", "application/vnd.comicbook-rar"),
    // subtitles
    ("srt", "application/x-subrip"),
    ("ass", "text/x-ssa"),
    ("ssa", "text/x-ssa"),
    ("vtt", "text/vtt"),
    // archives
    ("zip", "application/zip"),
    ("rar", "application/vnd.rar"),
    ("rar", "application/x-rar-compressed"),
    ("rar", "application/x-rar"),
    ("7z", "application/x-7z-compressed"),
    ("tar", "application/x-tar"),
    ("gz", "application/gzip"),
    ("gz", "application/x-gzip"),
    ("bz2", "application/x-bzip2"),
    ("xz", "application/x-xz"),
    ("zst", "application/zstd"),
    // programs, fonts, misc
    ("apk", "application/vnd.android.package-archive"),
    ("exe", "application/x-msdownload"),
    ("msi", "application/x-msi"),
    ("iso", "application/x-iso9660-image"),
    ("torrent", "application/x-bittorrent"),
    ("py", "text/x-python"),
    ("js", "text/javascript"),
    ("sh", "application/x-sh"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
];

/// Formats we recognise by name although we have no MIME type for them.
const EXTRA_EXTS: &[&str] = &[
    "sub", "idx", "nfo", "log", "ini", "cfg", "yml", "yaml", "toml", "c", "cpp", "h", "java", "kt",
    "rs", "go", "php", "rb", "pl", "lua", "bat", "cmd", "ps1", "jar", "deb", "rpm", "dmg", "pkg",
    "bin", "img", "cue", "m3u", "m3u8", "lrc", "ttml", "azw", "azw3", "fb2", "chm", "xlsm", "docm",
    "ppsx", "key", "pages", "numbers", "dwg", "dxf", "stl", "obj", "fbx", "blend", "ai", "eps",
    "indd", "xcf", "sqlite", "sqlite3", "db", "sql", "pem", "crt", "ipa", "xapk", "apks", "aab",
    "crx", "xpi", "vhd", "vmdk", "ova", "lz", "lzma", "z", "mk3d", "mxf", "rm", "dts", "ape", "wv",
    "tta", "m4b", "m4r", "mp2", "mpa", "weba", "jfif", "raw", "cr2", "nef", "arw", "dng",
];

/// Second half of `tar.<x>` names.
const COMPRESSORS: &[&str] = &["gz", "bz2", "xz", "zst", "lz", "lzma", "z"];

fn normalize(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// MIME types that carry no information.
fn is_generic(mime: &str) -> bool {
    matches!(
        mime,
        "" | "application/octet-stream"
            | "binary/octet-stream"
            | "application/x-binary"
            | "application/unknown"
    )
}

pub fn mime_from_ext(ext: &str) -> Option<&'static str> {
    let ext = ext.to_ascii_lowercase();
    TABLE
        .iter()
        .find(|(e, _)| *e == ext.as_str())
        .map(|(_, m)| *m)
}

/// Extension for a MIME type (`video/x-matroska` -> `mkv`); `None` for unknown or generic types.
pub fn ext_from_mime(mime: &str) -> Option<&'static str> {
    let m = normalize(mime);
    if is_generic(&m) {
        return None;
    }
    TABLE
        .iter()
        .find(|(_, t)| *t == m.as_str())
        .map(|(e, _)| *e)
}

pub fn is_known_ext(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    TABLE.iter().any(|(e, _)| *e == ext.as_str()) || EXTRA_EXTS.contains(&ext.as_str())
}

/// `("stem", "ext")` when `name` ends in `.<1-8 letters/digits>`.
fn split_last(name: &str) -> Option<(&str, &str)> {
    let (stem, ext) = name.rsplit_once('.')?;
    let ok = !stem.is_empty()
        && (1..=8).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then_some((stem, ext))
}

/// The extension of `name` if it is a format we know. Handles `tar.gz` and friends (returned lower-case).
pub fn file_ext(name: &str) -> Option<String> {
    let (stem, last) = split_last(name)?;
    let last = last.to_ascii_lowercase();
    if COMPRESSORS.contains(&last.as_str()) {
        if let Some((_, prev)) = split_last(stem) {
            if prev.eq_ignore_ascii_case("tar") {
                return Some(format!("tar.{last}"));
            }
        }
    }
    is_known_ext(&last).then_some(last)
}

/// Whatever follows the last dot, known or not (lower-case). For formats this bot has never heard of.
pub fn raw_ext(name: &str) -> Option<String> {
    split_last(name).map(|(_, e)| e.to_ascii_lowercase())
}

pub fn classify(mime: Option<&str>, name: &str) -> Kind {
    let by_mime = mime.map(normalize).filter(|m| !is_generic(m));
    let m = match by_mime {
        Some(m) => m,
        None => match file_ext(name).and_then(|e| mime_from_ext(&e)) {
            Some(m) => m.to_string(),
            None => return Kind::Other,
        },
    };
    if m.starts_with("video/") {
        Kind::Video
    } else if m.starts_with("audio/") {
        Kind::Audio
    } else if m.starts_with("image/") {
        Kind::Image
    } else if m == "application/pdf" {
        Kind::Pdf
    } else {
        Kind::Other
    }
}

/// MIME type for the renamed copy. The original type is kept (a video stays a video even when its name has no
/// extension); only when the extension was changed on purpose does the type follow the new name.
pub fn pick_mime(orig_mime: Option<&str>, orig_name: &str, new_name: &str) -> Option<String> {
    let orig = orig_mime.map(normalize).filter(|m| !is_generic(m));
    let new_ext = file_ext(new_name);
    let old_ext = file_ext(orig_name);
    if new_ext != old_ext {
        if let Some(m) = new_ext.as_deref().and_then(mime_from_ext) {
            return Some(m.to_string());
        }
    }
    if let Some(m) = orig {
        return Some(m);
    }
    new_ext
        .as_deref()
        .and_then(mime_from_ext)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(mime_from_ext("MKV"), Some("video/x-matroska"));
        assert_eq!(ext_from_mime("video/mp4; codecs=avc1"), Some("mp4"));
        assert_eq!(ext_from_mime("application/pdf"), Some("pdf"));
        assert_eq!(ext_from_mime("application/octet-stream"), None);
        assert_eq!(ext_from_mime("video/mpeg"), Some("mpg"));
    }

    #[test]
    fn extensions_of_names() {
        assert_eq!(file_ext("backup.TAR.GZ").as_deref(), Some("tar.gz"));
        assert_eq!(file_ext("a.gz").as_deref(), Some("gz"));
        assert_eq!(file_ext("Episode 01.MKV").as_deref(), Some("mkv"));
        assert_eq!(file_ext("Dr. Stone"), None);
        assert_eq!(file_ext("Naruto.S01E01.720p"), None);
        assert_eq!(raw_ext("thing.xyz").as_deref(), Some("xyz"));
        assert_eq!(raw_ext("no extension"), None);
    }

    #[test]
    fn kinds_and_thumbnails() {
        assert_eq!(
            classify(Some("application/octet-stream"), "a.mkv"),
            Kind::Video
        );
        assert_eq!(classify(Some("audio/mpeg"), "x"), Kind::Audio);
        assert_eq!(classify(None, "photo.PNG"), Kind::Image);
        assert_eq!(classify(Some("application/pdf"), "x"), Kind::Pdf);
        assert_eq!(classify(Some("text/plain"), "notes.txt"), Kind::Other);
        assert_eq!(classify(None, "x.zip"), Kind::Other);
        assert!(Kind::Video.takes_thumbnail() && Kind::Pdf.takes_thumbnail());
        assert!(!Kind::Other.takes_thumbnail());
    }

    #[test]
    fn mime_choice() {
        assert_eq!(
            pick_mime(Some("video/x-matroska"), "a.mkv", "B.mkv").as_deref(),
            Some("video/x-matroska")
        );
        assert_eq!(
            pick_mime(Some("video/mp4"), "a.mp4", "B.mkv").as_deref(),
            Some("video/x-matroska")
        );
        assert_eq!(
            pick_mime(Some("application/octet-stream"), "a.bin", "B.mp4").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(
            pick_mime(Some("video/mp4"), "episode", "B").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(pick_mime(None, "a.xyz", "B.xyz"), None);
    }
}
