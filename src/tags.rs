//! The tags that can be put in front of a file name: [E-1] [S-2] [1080p] [Hindi] [HEVC] [WEB-DL] [ESub] [1.46GB].
//!
//! `Episode` and `Season` are counters made by `naming`. Every other tag is read from the ORIGINAL file name
//! (Telegram gives no stream info without downloading the file), so a tag whose word is not in the original name
//! is simply left out for that file. Pure functions, unit-tested.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    Episode,
    Season,
    Quality,
    Audio,
    Codec,
    Source,
    Subs,
    Size,
}

/// Tags read from the original name / file, in the order they are written into the new name.
pub const EXTRA_ORDER: [Tag; 6] = [
    Tag::Quality,
    Tag::Audio,
    Tag::Codec,
    Tag::Source,
    Tag::Subs,
    Tag::Size,
];

impl Tag {
    const ALL: [Tag; 8] = [
        Tag::Episode,
        Tag::Season,
        Tag::Quality,
        Tag::Audio,
        Tag::Codec,
        Tag::Source,
        Tag::Subs,
        Tag::Size,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tag::Episode => "Episode",
            Tag::Season => "Season",
            Tag::Quality => "Quality",
            Tag::Audio => "Audio",
            Tag::Codec => "Codec",
            Tag::Source => "Source",
            Tag::Subs => "Subs",
            Tag::Size => "Size",
        }
    }

    /// Short id used in button data.
    pub fn code(self) -> &'static str {
        match self {
            Tag::Episode => "e",
            Tag::Season => "s",
            Tag::Quality => "q",
            Tag::Audio => "a",
            Tag::Codec => "c",
            Tag::Source => "r",
            Tag::Subs => "u",
            Tag::Size => "z",
        }
    }

    pub fn from_code(code: &str) -> Option<Tag> {
        Tag::ALL.iter().copied().find(|t| t.code() == code)
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// Which tags are switched on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TagSet(u16);

impl TagSet {
    pub fn has(self, tag: Tag) -> bool {
        self.0 & tag.bit() != 0
    }

    pub fn set(&mut self, tag: Tag, on: bool) {
        if on {
            self.0 |= tag.bit();
        } else {
            self.0 &= !tag.bit();
        }
    }

    pub fn toggle(&mut self, tag: Tag) {
        self.0 ^= tag.bit();
    }
}

/// What could be read from one original file name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Detected {
    pub quality: Option<String>,
    pub audio: Option<String>,
    pub codec: Option<String>,
    pub source: Option<String>,
    pub subs: Option<String>,
}

pub fn detect(name: &str) -> Detected {
    let lower = name.to_lowercase();
    let toks: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    // "WEB-DL", "Web.DL" and "WEBDL" all become "webdl"
    let squashed: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    let (audio, subs) = audio_and_subs(&toks);
    Detected {
        quality: quality(&toks),
        audio,
        codec: codec(&toks),
        source: source(&squashed),
        subs,
    }
}

/// The text to insert (`[1080p][Hindi]`, in the fixed order) and the selected tags that were not found.
pub fn extras(sel: TagSet, d: &Detected, size: usize) -> (String, Vec<Tag>) {
    let mut out = String::new();
    let mut missing = Vec::new();
    for tag in EXTRA_ORDER {
        if !sel.has(tag) {
            continue;
        }
        let value: Option<String> = match tag {
            Tag::Quality => d.quality.clone(),
            Tag::Audio => d.audio.clone(),
            Tag::Codec => d.codec.clone(),
            Tag::Source => d.source.clone(),
            Tag::Subs => d.subs.clone(),
            Tag::Size => Some(size_tag(size)),
            _ => None,
        };
        match value {
            Some(v) => {
                out.push('[');
                out.push_str(&v);
                out.push(']');
            }
            None => missing.push(tag),
        }
    }
    (out, missing)
}

pub fn size_tag(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2}GB", b / GB)
    } else if b >= MB {
        format!("{:.0}MB", b / MB)
    } else {
        format!("{:.0}KB", b / 1024.0)
    }
}

fn quality(toks: &[&str]) -> Option<String> {
    for t in toks {
        if let Some(num) = t.strip_suffix('p') {
            if let Ok(n) = num.parse::<u32>() {
                if matches!(
                    n,
                    144 | 240 | 360 | 480 | 540 | 576 | 720 | 1080 | 1440 | 2160 | 4320
                ) {
                    return Some(format!("{n}p"));
                }
            }
        }
        match *t {
            "4k" | "uhd" => return Some("4K".to_string()),
            "8k" => return Some("8K".to_string()),
            _ => {}
        }
    }
    None
}

fn codec(toks: &[&str]) -> Option<String> {
    for (i, t) in toks.iter().enumerate() {
        let hit = match *t {
            "x265" | "h265" | "hevc" => Some("HEVC"),
            "x264" | "h264" | "avc" => Some("x264"),
            "av1" => Some("AV1"),
            "vp9" => Some("VP9"),
            // "H.265" / "H 264" split by the dot
            "h" | "x" => match toks.get(i + 1) {
                Some(&"265") => Some("HEVC"),
                Some(&"264") => Some("x264"),
                _ => None,
            },
            _ => None,
        };
        if let Some(c) = hit {
            return Some(c.to_string());
        }
    }
    None
}

fn source(squashed: &str) -> Option<String> {
    const MAP: &[(&[&str], &str)] = &[
        (&["webdl"], "WEB-DL"),
        (&["webrip"], "WEBRip"),
        (&["bluray", "brrip", "bdrip"], "BluRay"),
        (&["hdrip"], "HDRip"),
        (&["hdtv"], "HDTV"),
        (&["dvdrip"], "DVDRip"),
        (&["hdcam", "camrip"], "CAM"),
    ];
    MAP.iter()
        .find(|(keys, _)| keys.iter().any(|k| squashed.contains(*k)))
        .map(|(_, label)| label.to_string())
}

/// Full language names anywhere; the short codes only where a clash with a title is unlikely.
const LANGS: &[(&str, &str)] = &[
    ("hindi", "Hindi"),
    ("hin", "Hindi"),
    ("english", "English"),
    ("eng", "English"),
    ("tamil", "Tamil"),
    ("telugu", "Telugu"),
    ("malayalam", "Malayalam"),
    ("kannada", "Kannada"),
    ("bengali", "Bengali"),
    ("marathi", "Marathi"),
    ("punjabi", "Punjabi"),
    ("japanese", "Japanese"),
    ("jpn", "Japanese"),
    ("jap", "Japanese"),
    ("korean", "Korean"),
    ("kor", "Korean"),
    ("chinese", "Chinese"),
    ("mandarin", "Chinese"),
    ("spanish", "Spanish"),
    ("french", "French"),
    ("german", "German"),
    ("russian", "Russian"),
    ("italian", "Italian"),
    ("arabic", "Arabic"),
    ("portuguese", "Portuguese"),
    ("thai", "Thai"),
    ("turkish", "Turkish"),
];

fn lang(tok: &str) -> Option<&'static str> {
    LANGS.iter().find(|(k, _)| *k == tok).map(|(_, v)| *v)
}

fn is_sub_word(tok: Option<&&str>) -> bool {
    matches!(
        tok,
        Some(&"sub") | Some(&"subs") | Some(&"subbed") | Some(&"subtitle") | Some(&"subtitles")
    )
}

/// (audio, subtitles). "Hindi English Subs" -> audio Hindi, subs ESub: a language followed by "sub" is a subtitle.
fn audio_and_subs(toks: &[&str]) -> (Option<String>, Option<String>) {
    let mut langs: Vec<&'static str> = Vec::new();
    let (mut dual, mut multi) = (false, false);
    let mut subs: Option<String> = None;

    for (i, t) in toks.iter().enumerate() {
        match *t {
            "dual" | "dualaudio" => {
                dual = true;
                continue;
            }
            "multi" | "multiaudio" => {
                multi = true;
                continue;
            }
            "esub" | "esubs" => {
                subs.get_or_insert_with(|| "ESub".to_string());
                continue;
            }
            "msub" | "msubs" | "multisub" | "multisubs" => {
                subs.get_or_insert_with(|| "MSub".to_string());
                continue;
            }
            "sub" | "subs" | "subbed" | "subtitle" | "subtitles" => {
                subs.get_or_insert_with(|| "Sub".to_string());
                continue;
            }
            _ => {}
        }
        if let Some(l) = lang(t) {
            if is_sub_word(toks.get(i + 1)) {
                let label = if l == "English" {
                    "ESub".to_string()
                } else {
                    format!("{l}Sub")
                };
                if subs.is_none() || subs.as_deref() == Some("Sub") {
                    subs = Some(label);
                }
            } else if !langs.contains(&l) {
                langs.push(l);
            }
        }
    }

    let audio = if multi || langs.len() > 3 {
        Some("Multi".to_string())
    } else if langs.len() >= 2 {
        Some(langs.join("-"))
    } else if dual {
        Some("Dual".to_string())
    } else {
        langs.first().map(|l| l.to_string())
    };
    (audio, subs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_release_name() {
        let d = detect("Naruto S01E05 1080p WEB-DL Hindi-English Dual Audio x265 ESub.mkv");
        assert_eq!(d.quality.as_deref(), Some("1080p"));
        assert_eq!(d.codec.as_deref(), Some("HEVC"));
        assert_eq!(d.source.as_deref(), Some("WEB-DL"));
        assert_eq!(d.audio.as_deref(), Some("Hindi-English"));
        assert_eq!(d.subs.as_deref(), Some("ESub"));
    }

    #[test]
    fn plain_movie_name() {
        let d = detect("Movie.2021.720p.BluRay.x264.mkv");
        assert_eq!(d.quality.as_deref(), Some("720p"));
        assert_eq!(d.source.as_deref(), Some("BluRay"));
        assert_eq!(d.codec.as_deref(), Some("x264"));
        assert_eq!(d.audio, None);
        assert_eq!(d.subs, None);
    }

    #[test]
    fn nothing_found() {
        assert_eq!(detect("Episode 01.mkv"), Detected::default());
        // short language codes that are also names must not match
        assert_eq!(detect("Ben 10 Tel Aviv.mkv").audio, None);
    }

    #[test]
    fn audio_variants() {
        assert_eq!(
            detect("Show 480p HDRip Hindi Dubbed").audio.as_deref(),
            Some("Hindi")
        );
        assert_eq!(detect("Show Dual Audio 4K").audio.as_deref(), Some("Dual"));
        assert_eq!(detect("Show Dual Audio 4K").quality.as_deref(), Some("4K"));
        assert_eq!(detect("Show Multi Audio").audio.as_deref(), Some("Multi"));
        assert_eq!(
            detect("Show [Hin+Eng]").audio.as_deref(),
            Some("Hindi-English")
        );
    }

    #[test]
    fn subtitles_are_not_audio() {
        let d = detect("Anime 1080p English Subs");
        assert_eq!(d.audio, None);
        assert_eq!(d.subs.as_deref(), Some("ESub"));
        let d = detect("Anime Japanese Hindi Spanish Subs");
        assert_eq!(d.subs.as_deref(), Some("SpanishSub"));
        assert_eq!(d.audio.as_deref(), Some("Japanese-Hindi"));
        assert_eq!(detect("Show MSubs").subs.as_deref(), Some("MSub"));
    }

    #[test]
    fn codec_with_dot() {
        assert_eq!(detect("Show H.265 10bit").codec.as_deref(), Some("HEVC"));
        assert_eq!(detect("Show H 264").codec.as_deref(), Some("x264"));
    }

    #[test]
    fn sizes() {
        assert_eq!(size_tag(1_567_000_000), "1.46GB");
        assert_eq!(size_tag(734_003_200), "700MB");
        assert_eq!(size_tag(20_480), "20KB");
    }

    #[test]
    fn extras_order_and_missing() {
        let d = detect("Show 720p Hindi");
        let mut sel = TagSet::default();
        for t in [Tag::Size, Tag::Audio, Tag::Quality, Tag::Codec] {
            sel.set(t, true);
        }
        let (text, missing) = extras(sel, &d, 734_003_200);
        assert_eq!(text, "[720p][Hindi][700MB]"); // fixed order, not the order they were switched on
        assert_eq!(missing, vec![Tag::Codec]);
    }

    #[test]
    fn tag_set_and_codes() {
        let mut s = TagSet::default();
        assert!(!s.has(Tag::Audio));
        s.toggle(Tag::Audio);
        assert!(s.has(Tag::Audio) && !s.has(Tag::Size));
        s.set(Tag::Audio, false);
        assert!(!s.has(Tag::Audio));
        for t in Tag::ALL {
            assert_eq!(Tag::from_code(t.code()), Some(t));
        }
        assert_eq!(Tag::from_code("zz"), None);
    }
}
