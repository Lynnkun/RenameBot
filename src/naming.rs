//! Name templates ({E:54}, {S}, {N}) and extension handling. Pure functions, unit-tested.
//!
//! Template tokens (case-insensitive):
//!   {E}      episode counter, starts at 1          -> [E-1], [E-2], ...
//!   {E:54}   episode counter starting at 54        -> [E-54], [E-55], ...
//!   {S}      season number                         -> [S-2]
//!   {N}      the original file name (no extension)
//!   {NAME}   the final new file name (captions only)
//! Extra tags ([1080p], [Hindi] ...) chosen with the buttons are inserted right after the leading [E-n][S-n] tags.
//! When the Season button is on and you did not place {S}, the season tag is added right after the episode tag,
//! so "{E:54} NAME" with season 2 becomes "[E-54][S-2] NAME".

use crate::kinds;

const MAX_NAME_BYTES: usize = 200;

#[derive(Debug, Clone)]
pub struct Template {
    /// Raw text with every token normalised to "{E}" / "{S}".
    text: String,
    ep_start: i64,
    has_e: bool,
    has_s: bool,
}

pub fn parse_template(raw: &str) -> Template {
    let mut out = String::new();
    let mut ep_start: Option<i64> = None;
    let (mut has_e, mut has_s) = (false, false);
    let mut rest = raw;

    while let Some(open) = rest.find('{') {
        let Some(close_rel) = rest[open..].find('}') else {
            break;
        };
        let close = open + close_rel;
        let inner = rest[open + 1..close].trim().to_ascii_lowercase();
        out.push_str(&rest[..open]);

        if inner == "e" {
            out.push_str("{E}");
            has_e = true;
        } else if let Some(n) = inner.strip_prefix("e:") {
            match n.trim().parse::<i64>() {
                Ok(v) if v >= 0 => {
                    out.push_str("{E}");
                    has_e = true;
                    ep_start.get_or_insert(v); // the first explicit start wins
                }
                _ => out.push_str(&rest[open..=close]), // {E:abc} stays literal
            }
        } else if inner == "s" {
            out.push_str("{S}");
            has_s = true;
        } else if inner == "n" {
            out.push_str("{N}");
        } else if inner == "name" {
            out.push_str("{NAME}");
        } else {
            out.push_str(&rest[open..=close]); // unknown {thing}: keep as typed
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);

    Template {
        text: out.trim().to_string(),
        ep_start: ep_start.unwrap_or(1),
        has_e,
        has_s,
    }
}

/// What one file contributes to its name.
pub struct Parts<'a> {
    /// 0-based position in the run.
    pub index: usize,
    /// `Some` = the season tag is on.
    pub season: Option<u32>,
    /// Add a counter automatically when the name has no {E} (the Episode button).
    pub episode: bool,
    /// Ready-made extra tags like "[1080p][Hindi]" ("" = none).
    pub extras: &'a str,
    /// The original file name without its extension.
    pub stem: &'a str,
}

struct Vals<'a> {
    episode: String,
    season: String,
    stem: &'a str,
    name: &'a str,
}

/// Replaces the normalised tokens in ONE pass, so text that came from a value is never treated as a token.
fn fill(text: &str, v: &Vals) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    while let Some(pos) = rest.find('{') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let (val, len): (Option<&str>, usize) = if tail.starts_with("{E}") {
            (Some(v.episode.as_str()), 3)
        } else if tail.starts_with("{S}") {
            (Some(v.season.as_str()), 3)
        } else if tail.starts_with("{NAME}") {
            (Some(v.name), 6)
        } else if tail.starts_with("{N}") {
            (Some(v.stem), 3)
        } else {
            (None, 1)
        };
        match val {
            Some(s) => out.push_str(s),
            None => out.push('{'),
        }
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

impl Template {
    fn vals<'a>(
        &self,
        index: usize,
        season: Option<u32>,
        stem: &'a str,
        name: &'a str,
    ) -> Vals<'a> {
        Vals {
            episode: format!("[E-{}]", self.ep_start + index as i64),
            season: season.map(|s| format!("[S-{s}]")).unwrap_or_default(),
            stem,
            name,
        }
    }

    /// Name (without extension) for one file of the run.
    pub fn render(&self, p: &Parts) -> String {
        let mut text = self.text.clone();
        if !self.has_e && p.episode {
            // no counter typed: add one in front so the files never share a name
            let auto_season = if p.season.is_some() && !self.has_s {
                "{S}"
            } else {
                ""
            };
            text = format!("{{E}}{auto_season} {text}");
        } else if p.season.is_some() && !self.has_s {
            if self.has_e {
                text = text.replacen("{E}", "{E}{S}", 1);
            } else {
                text = format!("{{S}} {text}");
            }
        }
        let core = fill(&text, &self.vals(p.index, p.season, p.stem, ""))
            .trim()
            .to_string();
        join_extras(&core, p.extras)
    }

    /// Caption text: the same tokens, but nothing is added automatically.
    pub fn render_free(&self, index: usize, season: Option<u32>, stem: &str, name: &str) -> String {
        fill(&self.text, &self.vals(index, season, stem, name))
            .trim()
            .to_string()
    }
}

/// Puts `extras` right after the leading [E-n]/[S-n] tags of `core` (or in front when there are none).
pub fn join_extras(core: &str, extras: &str) -> String {
    if extras.is_empty() {
        return core.to_string();
    }
    let k = leading_tags(core);
    let (head, tail) = core.split_at(k);
    if k > 0 {
        format!("{head}{extras}{tail}")
    } else if tail.is_empty() {
        extras.to_string()
    } else {
        format!("{extras} {tail}")
    }
}

/// Byte length of the run of `[E-12]` / `[S-2]` tags at the start of `s`.
fn leading_tags(s: &str) -> usize {
    let mut end = 0;
    loop {
        let Some(after_open) = s[end..].strip_prefix('[') else {
            break;
        };
        let Some(close) = after_open.find(']') else {
            break;
        };
        let inner = &after_open[..close];
        let ok = inner.len() > 2
            && (inner.starts_with("E-") || inner.starts_with("S-"))
            && inner[2..].chars().all(|c| c.is_ascii_digit());
        if !ok {
            break;
        }
        end += close + 2;
    }
    end
}

/// /single: the typed name as it is, only {N} is replaced by the original name.
pub fn fill_original(typed: &str, stem: &str) -> String {
    let lower = typed.to_ascii_lowercase(); // same byte offsets as `typed`
    let mut out = String::new();
    let mut at = 0;
    while let Some(p) = lower[at..].find("{n}") {
        out.push_str(&typed[at..at + p]);
        out.push_str(stem);
        at += p + 3;
    }
    out.push_str(&typed[at..]);
    out.trim().to_string()
}

/// The original file name without its (known) extension.
pub fn stem_of(name: &str) -> String {
    match kinds::file_ext(name) {
        Some(e) => split_suffix(name, &e)
            .map(|(stem, _)| stem)
            .unwrap_or(name)
            .to_string(),
        None => name.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtMode {
    /// Template runs: always end with the original extension.
    Always,
    /// /single: the name is used as typed; the original extension is added only when the name has none.
    IfMissing,
}

pub fn with_extension(base: &str, orig_name: &str, mime: Option<&str>, mode: ExtMode) -> String {
    let base = sanitize(base);
    let orig_ext = original_ext(orig_name, mime);

    let (stem, ext): (String, Option<String>) = match mode {
        ExtMode::Always => match &orig_ext {
            Some(e) => {
                let stem = split_suffix(&base, e)
                    .map(|(s, _)| s)
                    .unwrap_or(base.as_str());
                (stem.to_string(), Some(e.clone()))
            }
            None => (base.clone(), None),
        },
        ExtMode::IfMissing => {
            // the typed name already ends with the original extension, or with any format we recognise: keep it
            let typed = orig_ext
                .as_deref()
                .and_then(|e| split_suffix(&base, e))
                .or_else(|| kinds::file_ext(&base).and_then(|e| split_suffix(&base, &e)));
            match typed {
                Some((stem, e)) => (stem.to_string(), Some(e.to_string())),
                None => (base.clone(), orig_ext),
            }
        }
    };

    let room = MAX_NAME_BYTES.saturating_sub(ext.as_ref().map_or(0, |e| e.len() + 1));
    let mut stem = truncate_bytes(stem.trim(), room).trim().to_string();
    if stem.is_empty() || stem.chars().all(|c| c == '.') {
        stem = "file".into();
    }
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem,
    }
}

/// "dir/na:me" is legal in Telegram but breaks on disk/export, so path separators and control characters go.
/// Invisible direction/zero-width characters are removed too: "gpj.exe" with an RTL override displays as
/// "exe.jpg", which is how malicious files hide their real extension.
fn sanitize(name: &str) -> String {
    name.chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'   // zero-width space/joiners, LRM, RLM
            | '\u{202A}'..='\u{202E}' // bidi embeddings and overrides
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}' // bidi isolates
            | '\u{FEFF}'
    )
}

/// The extension the original file really has: a known one from its name (also `tar.gz`), else the one its MIME
/// type stands for (nameless voice notes, files uploaded without extension), else whatever follows the last dot.
fn original_ext(orig_name: &str, mime: Option<&str>) -> Option<String> {
    kinds::file_ext(orig_name)
        .or_else(|| mime.and_then(kinds::ext_from_mime).map(str::to_string))
        .or_else(|| kinds::raw_ext(orig_name))
}

/// Splits `base` into (stem, extension as typed) when it ends with `.<ext>` (case-insensitive).
fn split_suffix<'a>(base: &'a str, ext: &str) -> Option<(&'a str, &'a str)> {
    let cut = base.len().checked_sub(ext.len() + 1)?;
    if !base.is_char_boundary(cut) {
        return None;
    }
    let (stem, rest) = base.split_at(cut);
    let typed = rest.strip_prefix('.')?;
    typed.eq_ignore_ascii_case(ext).then_some((stem, typed))
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        tpl: &str,
        i: usize,
        season: Option<u32>,
        episode: bool,
        extras: &str,
        stem: &str,
    ) -> String {
        parse_template(tpl).render(&Parts {
            index: i,
            season,
            episode,
            extras,
            stem,
        })
    }

    fn season(tpl: &str, i: usize, s: u32) -> String {
        run(tpl, i, Some(s), true, "", "")
    }

    fn plain(tpl: &str, i: usize) -> String {
        run(tpl, i, None, true, "", "")
    }

    #[test]
    fn episode_counter_with_start() {
        assert_eq!(season("{E:54} NAME", 0, 2), "[E-54][S-2] NAME");
        assert_eq!(season("{E:54} NAME", 9, 2), "[E-63][S-2] NAME"); // ten files starting at 54 end at 63
    }

    #[test]
    fn episode_counter_default_start_and_separator() {
        assert_eq!(season("{E}_NAME", 0, 1), "[E-1][S-1]_NAME");
        assert_eq!(season("{e}_NAME", 2, 1), "[E-3][S-1]_NAME");
        assert_eq!(season("{E: 7} NAME", 1, 3), "[E-8][S-3] NAME");
    }

    #[test]
    fn counter_added_when_missing() {
        assert_eq!(season("NAME", 0, 3), "[E-1][S-3] NAME");
        assert_eq!(plain("NAME", 4), "[E-5] NAME");
    }

    #[test]
    fn explicit_season_placement() {
        assert_eq!(season("Show {S} {E:5}", 1, 2), "Show [S-2] [E-6]");
        assert_eq!(plain("Show {S} {E}", 0), "Show  [E-1]");
    }

    #[test]
    fn bulk_without_season() {
        assert_eq!(plain("{E:10} Title", 3), "[E-13] Title");
    }

    #[test]
    fn unknown_tokens_stay() {
        assert_eq!(
            season("{foo} {E:abc} x", 0, 1),
            "[E-1][S-1] {foo} {E:abc} x"
        );
    }

    #[test]
    fn extras_go_after_the_leading_tags() {
        let x = "[1080p][Hindi]";
        assert_eq!(
            run("{E:54} Show", 0, Some(2), true, x, ""),
            "[E-54][S-2][1080p][Hindi] Show"
        );
        assert_eq!(
            run("Show", 0, None, true, x, ""),
            "[E-1][1080p][Hindi] Show"
        );
        // Episode button off, nothing typed: no counter at all
        assert_eq!(run("Show", 3, None, false, x, ""), "[1080p][Hindi] Show");
        assert_eq!(run("Show", 3, Some(2), false, "", ""), "[S-2] Show");
        // a counter you typed yourself is always honoured
        assert_eq!(run("{E:5} Show", 1, None, false, "", ""), "[E-6] Show");
        // tags you placed in the middle stay there; extras then lead
        assert_eq!(
            run("Show {S} {E:5}", 1, Some(2), true, x, ""),
            "[1080p][Hindi] Show [S-2] [E-6]"
        );
    }

    #[test]
    fn original_name_token() {
        assert_eq!(
            run("{n}", 0, Some(1), true, "", "Episode 01"),
            "[E-1][S-1] Episode 01"
        );
        assert_eq!(run("{E} {N} END", 1, None, true, "", "Ep"), "[E-2] Ep END");
        // a value that looks like a token is not expanded again
        assert_eq!(run("{N}", 0, None, false, "", "A{E}B"), "A{E}B");
        assert_eq!(fill_original("{N} (fixed)", "Old"), "Old (fixed)");
        assert_eq!(fill_original("{E} stays", "Old"), "{E} stays");
        assert_eq!(stem_of("Episode 01.MKV"), "Episode 01");
        assert_eq!(stem_of("Dr. Stone"), "Dr. Stone");
        assert_eq!(stem_of("a.tar.gz"), "a");
    }

    #[test]
    fn caption_tokens() {
        let t = parse_template("Ep {E:7} of {S} - {N} -> {NAME}");
        assert_eq!(
            t.render_free(2, Some(3), "Old", "New.mkv"),
            "Ep [E-9] of [S-3] - Old -> New.mkv"
        );
        assert_eq!(
            parse_template("just text").render_free(0, None, "", ""),
            "just text"
        );
    }

    #[test]
    fn joins() {
        assert_eq!(join_extras("[E-1][S-2] X", "[a]"), "[E-1][S-2][a] X");
        assert_eq!(join_extras("X", "[a]"), "[a] X");
        assert_eq!(join_extras("", "[a]"), "[a]");
        assert_eq!(join_extras("[E-1] X", ""), "[E-1] X");
        assert_eq!(join_extras("[Group] X", "[a]"), "[a] [Group] X");
    }

    #[test]
    fn extension_always() {
        let n = |b: &str, o: &str, m| with_extension(b, o, m, ExtMode::Always);
        assert_eq!(
            n("[E-1][S-1] NAME", "Episode 01.mkv", None),
            "[E-1][S-1] NAME.mkv"
        );
        assert_eq!(n("NAME.MKV", "a.mkv", None), "NAME.mkv");
        assert_eq!(n("A", "", Some("video/x-matroska")), "A.mkv");
        assert_eq!(n("A", "", None), "A");
        // any other format keeps its own extension
        assert_eq!(n("Report", "scan.PDF", None), "Report.pdf");
        assert_eq!(n("Notes", "a.txt", Some("text/plain")), "Notes.txt");
        assert_eq!(n("NAME", "a.tar.gz", None), "NAME.tar.gz");
        assert_eq!(n("NAME.tar.gz", "a.tar.gz", None), "NAME.tar.gz");
        assert_eq!(n("Thing", "a.xyz", None), "Thing.xyz"); // a format nobody has heard of
                                                            // a name without a real extension: the MIME type decides
        assert_eq!(
            n("NAME", "Naruto.S01E01.720p", Some("video/mp4")),
            "NAME.mp4"
        );
    }

    #[test]
    fn extension_if_missing() {
        let n = |b: &str, o: &str| with_extension(b, o, None, ExtMode::IfMissing);
        assert_eq!(n("My Movie", "x.mp4"), "My Movie.mp4");
        assert_eq!(n("My Movie.mkv", "x.mp4"), "My Movie.mkv"); // as typed
        assert_eq!(n("Dr. Stone", "a.mkv"), "Dr. Stone.mkv");
        // formats outside any list must not get the extension twice
        assert_eq!(n("Report.pdf", "x.pdf"), "Report.pdf");
        assert_eq!(n("Report", "x.pdf"), "Report.pdf");
        assert_eq!(n("thing.xyz", "a.xyz"), "thing.xyz");
        assert_eq!(n("Archive", "a.tar.gz"), "Archive.tar.gz");
        assert_eq!(n("Archive.tar.gz", "a.tar.gz"), "Archive.tar.gz");
        assert_eq!(n("Vol.5", "a.mkv"), "Vol.5.mkv");
        assert_eq!(n(".mkv", "a.mkv"), "file.mkv");
    }

    #[test]
    fn strips_direction_overrides() {
        assert_eq!(
            with_extension("evil\u{202E}gpj.exe", "x.mkv", None, ExtMode::Always),
            "evilgpj.exe.mkv"
        );
    }

    #[test]
    fn sanitizes_and_limits() {
        assert_eq!(
            with_extension("a/b\\c", "x.mkv", None, ExtMode::Always),
            "a_b_c.mkv"
        );
        let long = "a".repeat(400);
        let out = with_extension(&long, "x.mkv", None, ExtMode::Always);
        assert!(out.len() <= MAX_NAME_BYTES && out.ends_with(".mkv"));
        let multibyte = "é".repeat(300);
        assert!(with_extension(&multibyte, "x.mp4", None, ExtMode::Always).len() <= MAX_NAME_BYTES);
    }
}
