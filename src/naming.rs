// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Output file naming: port of `CleanTitle`, `RemoveBrackets`, `GetRegion`
//! and `BuildOutputName` from `NsfwUtilities`.

use regex::Regex;
use std::path::{Component, Path};
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LanguageMode {
    Full,
    Short,
    None,
}

/// NACP language slots, in NACP order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    AmericanEnglish,
    BritishEnglish,
    Japanese,
    French,
    German,
    LatinAmericanSpanish,
    Spanish,
    Italian,
    Dutch,
    CanadianFrench,
    Portuguese,
    Russian,
    Korean,
    TraditionalChinese,
    SimplifiedChinese,
    BrazilianPortuguese,
}

impl Lang {
    pub const ALL: [Lang; 16] = [
        Lang::AmericanEnglish,
        Lang::BritishEnglish,
        Lang::Japanese,
        Lang::French,
        Lang::German,
        Lang::LatinAmericanSpanish,
        Lang::Spanish,
        Lang::Italian,
        Lang::Dutch,
        Lang::CanadianFrench,
        Lang::Portuguese,
        Lang::Russian,
        Lang::Korean,
        Lang::TraditionalChinese,
        Lang::SimplifiedChinese,
        Lang::BrazilianPortuguese,
    ];

    pub fn full(self) -> &'static str {
        use Lang::*;
        match self {
            AmericanEnglish => "English (America)",
            BritishEnglish => "English (Great Britain)",
            Japanese => "Japanese",
            French => "French (France)",
            CanadianFrench => "French (Canada)",
            German => "German",
            Italian => "Italian",
            Spanish => "Spanish (Spain)",
            LatinAmericanSpanish => "Spanish (Latin America)",
            SimplifiedChinese => "Chinese (Simplified)",
            TraditionalChinese => "Chinese (Traditional)",
            Korean => "Korean",
            Dutch => "Dutch",
            Portuguese => "Portuguese (Portugal)",
            BrazilianPortuguese => "Portuguese (Brazil)",
            Russian => "Russian",
        }
    }

    pub fn full_short(self) -> &'static str {
        use Lang::*;
        match self {
            AmericanEnglish => "En-US",
            BritishEnglish => "En-GB",
            Japanese => "Ja-JP",
            French => "Fr-FR",
            CanadianFrench => "Fr-CA",
            German => "De-DE",
            Italian => "It-IT",
            Spanish => "Es-ES",
            LatinAmericanSpanish => "Es-XL",
            SimplifiedChinese => "Zh-Hans",
            TraditionalChinese => "Zh-Hant",
            Korean => "Ko-KR",
            Dutch => "Nl-NL",
            Portuguese => "Pt-PT",
            BrazilianPortuguese => "Pt-BR",
            Russian => "Ru-RU",
        }
    }

    pub fn short(self) -> &'static str {
        use Lang::*;
        match self {
            AmericanEnglish | BritishEnglish => "En",
            Japanese => "Ja",
            French | CanadianFrench => "Fr",
            German => "De",
            Italian => "It",
            Spanish | LatinAmericanSpanish => "Es",
            SimplifiedChinese => "Zh-Hans",
            TraditionalChinese => "Zh-Hant",
            Korean => "Ko",
            Dutch => "Nl",
            Portuguese | BrazilianPortuguese => "Pt",
            Russian => "Ru",
        }
    }
}

/// Distinct short language codes, first occurrence order.
pub fn languages_short(titles: &[Lang]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for t in titles {
        if !v.contains(&t.short()) {
            v.push(t.short());
        }
    }
    v
}

const CLEAN_REPLACEMENTS: &[(&str, &str)] = &[
    ("  ", " "),
    ("\u{3000}", " "),
    (" <", " - "),
    (">", ""),
    ("\u{2013}", "-"),
    ("\u{2014}", "-"),
    ("\u{201C}", "'"),
    ("*", ""),
    ("\u{201D}", "'"),
    ("/", "-"),
    ("①", "1"),
    ("②", "2"),
    ("③", "3"),
    ("④", "4"),
    ("⑤", "5"),
    ("⑥", "6"),
    ("⑦", "7"),
    ("⑧", "8"),
    ("⑨", "9"),
    ("⑩", "10"),
    ("⑪", "11"),
    ("⑫", "12"),
    ("⑬", "13"),
    ("⑭", "14"),
    ("⑮", "15"),
    ("⑯", "16"),
    ("⑰", "17"),
    ("⑱", "18"),
    ("⑲", "19"),
    ("⑳", "20"),
    ("１", "1"),
    ("２", "2"),
    ("３", "3"),
    ("４", "4"),
    ("５", "5"),
    ("６", "6"),
    ("７", "7"),
    ("８", "8"),
    ("９", "9"),
    ("０", "0"),
    ("Ａ", "A"),
    ("Ｂ", "B"),
    ("Ｃ", "C"),
    ("Ｄ", "D"),
    ("Ｅ", "E"),
    ("Ｆ", "F"),
    ("Ｇ", "G"),
    ("Ｈ", "H"),
    ("Ｉ", "I"),
    ("Ｊ", "J"),
    ("Ｋ", "K"),
    ("Ｌ", "L"),
    ("Ｍ", "M"),
    ("Ｎ", "N"),
    ("Ｏ", "O"),
    ("Ｐ", "P"),
    ("Ｑ", "Q"),
    ("Ｒ", "R"),
    ("Ｓ", "S"),
    ("Ｔ", "T"),
    ("Ｕ", "U"),
    ("Ｖ", "V"),
    ("Ｗ", "W"),
    ("Ｘ", "X"),
    ("Ｙ", "Y"),
    ("Ｚ", "Z"),
    ("ａ", "a"),
    ("ｂ", "b"),
    ("ｃ", "c"),
    ("ｄ", "d"),
    ("ｅ", "e"),
    ("ｆ", "f"),
    ("ｇ", "g"),
    ("ｈ", "h"),
    ("ｉ", "i"),
    ("ｊ", "j"),
    ("ｋ", "k"),
    ("ｌ", "l"),
    ("ｍ", "m"),
    ("ｎ", "n"),
    ("ｏ", "o"),
    ("ｐ", "p"),
    ("ｑ", "q"),
    ("ｒ", "r"),
    ("ｓ", "s"),
    ("ｔ", "t"),
    ("ｕ", "u"),
    ("ｖ", "v"),
    ("ｗ", "w"),
    ("ｘ", "x"),
    ("ｙ", "y"),
    ("ｚ", "z"),
    ("×", " x "),
    ("Ⅰ", "I"),
    ("Ⅱ", "II"),
    ("Ⅲ", "III"),
    ("Ⅳ", "IV"),
    ("Ⅴ", "V"),
    ("Ⅵ", "VI"),
    ("Ⅶ", "VII"),
    ("Ⅷ", "VIII"),
    ("Ⅸ", "IX"),
    ("Ⅹ", "X"),
    ("Ⅺ", "XI"),
    ("Ⅻ", "XII"),
    ("Ⅼ", "L"),
    ("Ⅽ", "C"),
    ("Ⅾ", "D"),
    ("Ⅿ", "M"),
    ("ⅰ", "i"),
    ("ⅱ", "ii"),
    ("ⅲ", "iii"),
    ("ⅳ", "iv"),
    ("ⅴ", "v"),
    ("ⅵ", "vi"),
    ("ⅶ", "vii"),
    ("ⅷ", "viii"),
    ("ⅸ", "ix"),
    ("ⅹ", "x"),
    ("ⅺ", "xi"),
    ("ⅻ", "xii"),
    ("ⅼ", "l"),
    ("ⅽ", "c"),
    ("ⅾ", "d"),
    ("ⅿ", "m"),
    ("™", ""),
    ("®", ""),
    ("©", ""),
    ("！", "!"),
    ("？", "?"),
    ("：", ":"),
    ("；", ";"),
    ("，", ","),
    ("、", ","),
    ("。", "."),
    ("「", "'"),
    ("」", "'"),
    ("︰", ":"),
    ("\u{ff5e}", "-"),
    (":||", "-"),
    (": ", " - "),
    (":", "-"),
    (" ~", " - "),
    ("~ ", " - "),
    (" - - ", " - "),
    ("- - ", " - "),
    (" -  - ", " - "),
    (" - ：", " - "),
    ("|", ""),
    ("\"", ""),
    ("\u{2122}", ""),
    ("\u{00ae}", ""),
    ("\u{00a9}", ""),
    ("!?", ""),
    ("?", ""),
    (" - - ", " - "),
    ("（", " ("),
    ("）", ") "),
    ("） (", ") ("),
    (" dlc", " DLC"),
    ("((((", "("),
    ("))))", ")"),
    ("(((", "("),
    (")))", ")"),
    ("((", "("),
    ("))", ")"),
    ("  ", " "),
];

/// .NET `string.ReplaceLineEndings("")`.
pub fn remove_line_endings(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                '\r' | '\n' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
            )
        })
        .collect()
}

pub fn clean_title(title: &str) -> String {
    let mut s = remove_line_endings(title);
    for (from, to) in CLEAN_REPLACEMENTS {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }
    let mut s = s.trim_end().to_string();
    if let Some(t) = s.strip_suffix(" -") {
        s = t.to_string();
    }
    if let Some(t) = s.strip_prefix('-') {
        s = t.to_string();
    }
    crate::diacritics::remove(&s)
}

/// True if `name` is a single plain file name, so joining it to a directory
/// cannot leave that directory. On Windows this rejects `\`, drive and root
/// prefixes, which the title cleanup does not strip.
pub fn is_plain_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

pub fn remove_brackets(input: &str) -> String {
    input
        .replace(" (", " - ")
        .replace("( ", " - ")
        .replace('(', " - ")
        .replace([')', '[', ']', '}', '{', '<', '>'], " ")
        .trim()
        .to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    World,
    Europe,
    Asia,
    Japan,
    Korea,
    China,
    Russia,
    Usa,
    UnitedKingdom,
    LatinAmerica,
    Brazil,
    Netherlands,
    Portugal,
    France,
    Germany,
    Italy,
    Spain,
    Taiwan,
    Unknown,
}

fn get_region(titles: &[Lang]) -> Region {
    use Lang::*;
    if titles.is_empty() {
        return Region::Unknown;
    }
    if titles.len() == 1 {
        let r = match titles[0] {
            AmericanEnglish => Region::Usa,
            Japanese => Region::Japan,
            Korean => Region::Korea,
            Russian => Region::Russia,
            TraditionalChinese => Region::Taiwan,
            SimplifiedChinese => Region::China,
            BritishEnglish => Region::UnitedKingdom,
            LatinAmericanSpanish => Region::LatinAmerica,
            BrazilianPortuguese => Region::Brazil,
            Dutch => Region::Netherlands,
            Portuguese => Region::Portugal,
            French => Region::France,
            German => Region::Germany,
            Italian => Region::Italy,
            Spanish => Region::Spain,
            CanadianFrench => Region::Unknown,
        };
        if r != Region::Unknown {
            return r;
        }
    }
    let any = |set: &[Lang]| titles.iter().any(|t| set.contains(t));
    let mut region = Region::Unknown;

    if any(&[TraditionalChinese, SimplifiedChinese]) {
        region = Region::China;
        if any(&[Japanese, Korean]) {
            region = Region::Asia;
        }
        if any(&[
            AmericanEnglish,
            BritishEnglish,
            French,
            German,
            Italian,
            Spanish,
            Dutch,
            Portuguese,
        ]) {
            region = Region::World;
        }
    }
    if region == Region::Unknown
        && any(&[
            BritishEnglish,
            French,
            German,
            Italian,
            Spanish,
            Dutch,
            Portuguese,
        ])
    {
        region = Region::Europe;
        if any(&[
            AmericanEnglish,
            Japanese,
            Korean,
            Russian,
            TraditionalChinese,
            SimplifiedChinese,
            LatinAmericanSpanish,
            BrazilianPortuguese,
            CanadianFrench,
        ]) {
            region = Region::World;
        }
    }
    if region == Region::Unknown && any(&[AmericanEnglish]) {
        region = Region::Usa;
        if any(&[
            Japanese,
            Korean,
            Russian,
            TraditionalChinese,
            SimplifiedChinese,
            LatinAmericanSpanish,
            BrazilianPortuguese,
            CanadianFrench,
        ]) {
            region = Region::World;
        }
    }
    region
}

/// Enum name, as printed by .NET `$"{region}"`.
fn region_name(r: Region) -> &'static str {
    match r {
        Region::World => "World",
        Region::Europe => "Europe",
        Region::Asia => "Asia",
        Region::Japan => "Japan",
        Region::Korea => "Korea",
        Region::China => "China",
        Region::Russia => "Russia",
        Region::Usa => "USA",
        Region::UnitedKingdom => "UnitedKingdom",
        Region::LatinAmerica => "LatinAmerica",
        Region::Brazil => "Brazil",
        Region::Netherlands => "Netherlands",
        Region::Portugal => "Portugal",
        Region::France => "France",
        Region::Germany => "Germany",
        Region::Italy => "Italy",
        Region::Spain => "Spain",
        Region::Taiwan => "Taiwan",
        Region::Unknown => "Unknown",
    }
}

fn display_region(r: Region) -> &'static str {
    match r {
        Region::Usa => "(USA)",
        Region::Europe => "(Europe)",
        Region::Asia => "(Asia)",
        Region::Japan => "(Japan)",
        Region::Korea => "(Korea)",
        Region::China => "(China)",
        Region::Russia => "(Russia)",
        Region::UnitedKingdom => "(United Kingdom)",
        Region::World => "(World)",
        Region::LatinAmerica => "(Latin America)",
        Region::Brazil => "(Brazil)",
        Region::Netherlands => "(Netherlands)",
        Region::Portugal => "(Portugal)",
        Region::France => "(France)",
        Region::Germany => "(Germany)",
        Region::Italy => "(Italy)",
        Region::Spain => "(Spain)",
        Region::Taiwan => "(Taiwan)",
        Region::Unknown => "",
    }
}

fn char_eq_ignore_case(a: char, b: char) -> bool {
    a == b || a.to_uppercase().eq(b.to_uppercase())
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    let mut it = s.chars();
    prefix
        .chars()
        .all(|p| it.next().is_some_and(|c| char_eq_ignore_case(c, p)))
}

/// .NET `string.Equals(other, StringComparison.InvariantCultureIgnoreCase)`
/// under invariant globalization.
pub fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars()
            .zip(b.chars())
            .all(|(x, y)| char_eq_ignore_case(x, y))
}

/// .NET `string.Contains(value, StringComparison.InvariantCultureIgnoreCase)`
/// under invariant globalization.
fn contains_ignore_case(s: &str, value: &str) -> bool {
    s.char_indices()
        .any(|(i, _)| starts_with_ignore_case(&s[i..], value))
        || value.is_empty()
}

/// .NET `string.Replace(old, new, StringComparison.InvariantCultureIgnoreCase)`
/// under invariant globalization (ordinal ignore-case, all occurrences).
pub fn replace_ignore_case(s: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return s.to_string();
    }
    let from_len = from.chars().count();
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if starts_with_ignore_case(rest, from) {
            out.push_str(to);
            let skip: usize = rest.chars().take(from_len).map(char::len_utf8).sum();
            rest = &rest[skip..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// .NET `Split(" - ", RemoveEmptyEntries | TrimEntries)`.
fn split_parts(s: &str) -> Vec<&str> {
    s.split(" - ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// .NET `Split(" - ", RemoveEmptyEntries | TrimEntries).First()`.
fn first_part(s: &str) -> String {
    split_parts(s).first().copied().unwrap_or("").to_string()
}

/// Move a leading article after the first title part
/// ("The Foo - Bar" becomes "Foo, The  - Bar").
fn move_article(s: String, article: &str) -> String {
    if !starts_with_ignore_case(&s, article) {
        return s;
    }
    let rest: String = s.chars().skip(article.chars().count()).collect();
    let first = first_part(&rest);
    let word = article.trim_end();
    replace_ignore_case(&rest, &first, &format!("{first}, {word} "))
}

fn drop_last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().take(count.saturating_sub(n)).collect()
}

static CLOSE_HYPHEN_WRAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-\S.*?\S-").unwrap());

pub struct NameInputs<'a> {
    pub language_mode: LanguageMode,
    pub titles: &'a [Lang],
    pub display_title: &'a str,
    pub display_version: &'a str,
    pub is_patch: bool,
    pub title_version: &'a str,
    pub display_type_short: &'a str,
    pub title_id: &'a str,
    pub is_demo: bool,
    pub is_dlc: bool,
    /// Parent (base) title of a DLC, from TitleDB.
    pub display_parent_title: Option<&'a str>,
    /// Parent languages of a DLC, from TitleDB.
    pub parent_languages: &'a [Lang],
    pub display_parent_languages: &'a str,
    /// Region from TitleDB; empty when unknown.
    pub distribution_region: &'a str,
    pub keep_name: bool,
}

/// Port of `NsfwUtilities.BuildOutputName`.
pub fn build_output_name(n: &NameInputs) -> String {
    let list = match n.language_mode {
        LanguageMode::Full => n
            .titles
            .iter()
            .map(|t| t.full_short())
            .collect::<Vec<_>>()
            .join(","),
        LanguageMode::Short => languages_short(n.titles).join(","),
        LanguageMode::None => String::new(),
    };
    let (mut language_list, mut region) = if n.language_mode == LanguageMode::None {
        (String::new(), String::new())
    } else {
        // An empty list becomes "((Unknown))"; clean_title later collapses the parens.
        let list = if list.trim().is_empty() {
            "(Unknown)".to_string()
        } else {
            list
        };
        let region = if n.distribution_region.trim().is_empty() {
            display_region(get_region(n.titles)).to_string()
        } else {
            format!("({})", n.distribution_region)
        };
        (format!("({list})"), region)
    };

    let mut clean = clean_title(n.display_title);
    if clean.trim().is_empty() {
        clean = String::new();
    }
    clean = clean.replace(n.title_id, "");
    if let Some(t) = clean.strip_suffix("v0") {
        clean = t.to_string();
    }

    let mut is_demo = n.is_demo;
    if let Some(t) = clean.strip_suffix("体験版") {
        clean = t.to_string();
        is_demo = true;
    }
    if let Some(t) = clean.strip_prefix("【体験版】") {
        clean = t.to_string();
        is_demo = true;
    }
    if let Some(t) = clean.strip_suffix("【体験版】") {
        clean = t.to_string();
        is_demo = true;
    }
    // (suffix to match, number of chars removed). The removal counts are
    // kept as in the original even where they differ from the suffix length.
    for (suffix, cut) in [
        (" (DEMO)", 7),
        (" DEMO ", 6),
        (" DEMO", 5),
        (" [DEMO VERSION]", 15),
        (" (DEMO VERSION)", 15),
        (" DEMO VERSION", 13),
        (" DEMO VER.", 10),
        (" TRIAL VERSION", 13),
        (" [TRIAL VERSION]", 15),
        (" TRIAL EDITION", 14),
        (" FREE TRIAL", 10),
    ] {
        if clean.to_uppercase().ends_with(suffix) {
            clean = drop_last_chars(&clean, cut);
            is_demo = true;
        }
    }

    let mut demo = "";
    if is_demo {
        if let Some(t) = clean.strip_suffix(" -") {
            clean = t.to_string();
        }
        if let Some(t) = clean.strip_suffix(" - ") {
            clean = t.to_string();
        }
        demo = "(Demo)";
    }

    clean = move_article(clean, "The ");
    clean = move_article(clean, "A ");

    if let Some(m) = CLOSE_HYPHEN_WRAP.find(&clean) {
        let value = m.as_str().to_string();
        let inner = &value[1..value.len() - 1];
        clean = replace_ignore_case(&clean, &value, &format!(" - {inner}"));
    }
    if let Some(t) = clean.strip_suffix('-') {
        clean = t.to_string();
    }

    let tail = format!(
        "[{}][{}][{}]",
        n.title_id, n.title_version, n.display_type_short
    );
    if n.is_patch {
        return clean_title(&format!(
            "{} {region}{language_list}{demo}[{}]{tail}",
            remove_brackets(&clean),
            n.display_version
        ));
    }

    if let Some(parent) = n.display_parent_title.filter(|p| n.is_dlc && !p.is_empty()) {
        if !n.parent_languages.is_empty() {
            language_list = n.display_parent_languages.to_string();
            region = if n.language_mode != LanguageMode::None {
                format!("({})", region_name(get_region(n.parent_languages)))
            } else {
                String::new()
            };
            if !region.trim().is_empty() && !n.distribution_region.trim().is_empty() {
                region = format!("({})", n.distribution_region);
            }
            if n.language_mode == LanguageMode::None {
                language_list = String::new();
            }
            if !language_list.is_empty() {
                language_list = format!("({})", language_list.replace("Zh,Zh", "Zh-Hans,Zh-Hant"));
            }
        }

        let mut clean_parent = clean_title(parent);
        if clean_parent.trim().is_empty() {
            clean_parent = String::new();
        }
        for part in split_parts(&clean_parent) {
            clean = replace_ignore_case(&clean, part, "");
        }
        clean = clean_title(&clean);
        for part in split_parts(&clean.clone()) {
            if contains_ignore_case(&clean_parent, part) {
                clean = clean_title(&replace_ignore_case(&clean, part, ""));
            }
        }

        let mut final_title = if n.keep_name {
            clean_title(&format!(
                "{} {region}{language_list}",
                remove_brackets(n.display_title)
            ))
        } else {
            clean_title(&format!(
                "{} - {} {region}{language_list}",
                remove_brackets(&clean_parent),
                remove_brackets(&clean)
            ))
        };
        final_title = move_article(final_title, "The ");
        final_title = move_article(final_title, "A ");
        return clean_title(&format!("{final_title}{tail}"));
    }

    let version = if n.display_version == "UNKNOWN" {
        String::new()
    } else {
        format!("[{}]", n.display_version)
    };
    clean_title(&format!(
        "{} {region}{language_list}{demo}{version}{tail}",
        remove_brackets(&clean)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Lang::*;

    fn base(title: &str, langs: &[Lang], version: &str, id: &str) -> String {
        build_output_name(&NameInputs {
            language_mode: LanguageMode::Full,
            titles: langs,
            display_title: title,
            display_version: version,
            is_patch: false,
            title_version: "v0",
            display_type_short: "BASE",
            title_id: id,
            is_demo: false,
            is_dlc: false,
            display_parent_title: None,
            parent_languages: &[],
            display_parent_languages: "UNKNOWN",
            distribution_region: "",
            keep_name: false,
        })
    }

    fn dlc(
        title: &str,
        parent: &str,
        parent_langs: &[Lang],
        display_parent_langs: &str,
        dist: &str,
    ) -> String {
        build_output_name(&NameInputs {
            language_mode: LanguageMode::Full,
            titles: &[AmericanEnglish],
            display_title: title,
            display_version: "1.0.0",
            is_patch: false,
            title_version: "v0",
            display_type_short: "DLC",
            title_id: "0100000000001001",
            is_demo: false,
            is_dlc: true,
            display_parent_title: Some(parent),
            parent_languages: parent_langs,
            display_parent_languages: display_parent_langs,
            distribution_region: dist,
            keep_name: false,
        })
    }

    #[test]
    fn dlc_names() {
        // Parent title parts are stripped from the DLC title.
        assert_eq!(
            dlc("Foo Game - Bonus Pack", "Foo Game", &[], "UNKNOWN", ""),
            "Foo Game - Bonus Pack (USA)(En-US)[0100000000001001][v0][DLC]"
        );
        // Parent languages replace the list; region printed as enum name.
        assert_eq!(
            dlc(
                "Bonus Pack",
                "Foo Game",
                &[AmericanEnglish, SimplifiedChinese, SimplifiedChinese],
                "En,Zh,Zh",
                ""
            ),
            "Foo Game - Bonus Pack (World)(En,Zh-Hans,Zh-Hant)[0100000000001001][v0][DLC]"
        );
        assert_eq!(
            dlc("Bonus Pack", "Foo Game", &[BritishEnglish], "En", ""),
            "Foo Game - Bonus Pack (UnitedKingdom)(En)[0100000000001001][v0][DLC]"
        );
        // Distribution region wins over the parent language region.
        assert_eq!(
            dlc(
                "Bonus Pack",
                "The Foo Game",
                &[AmericanEnglish],
                "En",
                "Europe"
            ),
            "Foo Game, The - Bonus Pack (Europe)(En)[0100000000001001][v0][DLC]"
        );
    }

    #[test]
    fn golden_names() {
        assert_eq!(
            base(
                "Animal Crossing: New Horizons",
                &[
                    AmericanEnglish,
                    BritishEnglish,
                    Japanese,
                    French,
                    German,
                    LatinAmericanSpanish,
                    Spanish,
                    Italian,
                    Dutch,
                    CanadianFrench,
                    Russian,
                    Korean,
                    TraditionalChinese,
                    SimplifiedChinese
                ],
                "1.0.0",
                "01006F8002326000"
            ),
            "Animal Crossing - New Horizons (World)(En-US,En-GB,Ja-JP,Fr-FR,De-DE,Es-XL,Es-ES,It-IT,Nl-NL,Fr-CA,Ru-RU,Ko-KR,Zh-Hant,Zh-Hans)[1.0.0][01006F8002326000][v0][BASE]"
        );
        assert_eq!(
            base(
                "Bayonetta 2",
                &[AmericanEnglish, Japanese, French, German, Spanish, Italian],
                "1.0",
                "01007960049A0000"
            ),
            "Bayonetta 2 (World)(En-US,Ja-JP,Fr-FR,De-DE,Es-ES,It-IT)[1.0][01007960049A0000][v0][BASE]"
        );
        assert_eq!(
            base(
                "Beyond Good & Evil - 20th Anniversary Edition",
                &[
                    AmericanEnglish,
                    Japanese,
                    French,
                    German,
                    Spanish,
                    Italian,
                    Dutch,
                    Russian,
                    Korean,
                    TraditionalChinese,
                    SimplifiedChinese,
                    BrazilianPortuguese
                ],
                "1.0.0",
                "0100E0A01DD20000"
            ),
            "Beyond Good & Evil - 20th Anniversary Edition (World)(En-US,Ja-JP,Fr-FR,De-DE,Es-ES,It-IT,Nl-NL,Ru-RU,Ko-KR,Zh-Hant,Zh-Hans,Pt-BR)[1.0.0][0100E0A01DD20000][v0][BASE]"
        );
    }

    #[test]
    fn distribution_region_overrides() {
        assert_eq!(
            build_output_name(&NameInputs {
                language_mode: LanguageMode::Full,
                titles: &[Japanese],
                display_title: "Foo",
                display_version: "1.0",
                is_patch: false,
                title_version: "v0",
                display_type_short: "BASE",
                title_id: "X",
                is_demo: false,
                is_dlc: false,
                display_parent_title: None,
                parent_languages: &[],
                display_parent_languages: "UNKNOWN",
                distribution_region: "North America",
                keep_name: false,
            }),
            "Foo (North America)(Ja-JP)[1.0][X][v0][BASE]"
        );
    }

    #[test]
    fn articles_and_cleanup() {
        assert_eq!(
            base(
                "The Legend of Zelda: Breath of the Wild",
                &[AmericanEnglish],
                "1.0.0",
                "01007EF00011E000"
            ),
            "Legend of Zelda, The - Breath of the Wild (USA)(En-US)[1.0.0][01007EF00011E000][v0][BASE]"
        );
        assert_eq!(clean_title("Pokémon™ Sword"), "Pokemon Sword");
        assert_eq!(clean_title("ポケモン ガ"), "ポケモン ガ");
        assert_eq!(clean_title("Łódź æ Straße"), "Lodz ae Straße");
        assert_eq!(remove_brackets("Foo (Bar) [x]"), "Foo - Bar   x");
        assert_eq!(
            base("Demon Slayer -Kimetsu no Yaiba-", &[Japanese], "1.0", "X"),
            "Demon Slayer - Kimetsu no Yaiba (Japan)(Ja-JP)[1.0][X][v0][BASE]"
        );
    }

    #[test]
    fn plain_file_name() {
        assert!(is_plain_file_name("..x.nsp"));
        assert!(is_plain_file_name(
            "Super Game (USA)(En-US)[1.0.0][0100000000010000][v0][BASE].nsp"
        ));
        assert!(!is_plain_file_name(""));
        assert!(!is_plain_file_name(".."));
        assert!(!is_plain_file_name("../evil.nsp"));
        assert!(!is_plain_file_name("/evil.nsp"));
        assert!(!is_plain_file_name("a/b.nsp"));
    }

    #[cfg(windows)]
    #[test]
    fn plain_file_name_windows() {
        assert!(!is_plain_file_name("..\\evil.nsp"));
        assert!(!is_plain_file_name("\\evil.nsp"));
        assert!(!is_plain_file_name("C:evil.nsp"));
        assert!(!is_plain_file_name("a\\b.nsp"));
    }
}
