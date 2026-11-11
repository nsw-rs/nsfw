// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `query`: print the TitleDB info of a title ID. The original draws nested
//! tables; this prints the same content as `key : value` rows.

use anyhow::Result;
use std::path::Path;

use crate::log::{line, olive};
use crate::naming::remove_line_endings;
use crate::titledb::{AMERICAS, ASIA, EUROPE, TitleDb};

pub fn run(titledb: &Path, title_id: &str) -> Result<()> {
    let mut query = title_id.to_string();
    if let Some(base) = query.strip_suffix("800") {
        line("This is an update title ID. Searching parent title ID..");
        query = format!("{base}000");
    }

    let db = TitleDb::open(titledb)?;
    let results = db.game_info(&query)?;
    let Some(first) = results.first() else {
        line("No results found.");
        return Ok(());
    };

    line("");
    // Continuation lines of a multi-line value line up under its first line.
    let top = |k: &str, v: &str| {
        let mut lines = v.split('\n').map(|l| l.trim_end_matches('\r'));
        line(&format!("  {k:<11} : {}", lines.next().unwrap_or_default()));
        for l in lines {
            line(&format!("                {l}"));
        }
    };
    top("Name", &olive(first.name.as_deref().unwrap_or_default()));
    top("Publisher", first.publisher.as_deref().unwrap_or("Unknown"));
    top(
        "Description",
        first.description.as_deref().unwrap_or("Unknown"),
    );

    // The original looks these up with the ID as given, not the parent ID.
    let other;
    let titles = if query == title_id {
        &results
    } else {
        other = db.game_info(title_id)?;
        &other
    };
    if !titles.is_empty() {
        line("  Regional Titles");
        let mut seen = Vec::new();
        for t in titles {
            if seen.contains(&&t.region_language) {
                continue;
            }
            seen.push(&t.region_language);
            let lang = t.region_language.as_deref().unwrap_or_default();
            let name = remove_line_endings(t.name.as_deref().unwrap_or_default());
            line(&format!("    {:<5} : {name}", lang.to_uppercase()));
        }
    }

    line("  eShop Variations");
    let mut seen = Vec::new();
    for r in &results {
        if seen.contains(&r.nsu_id) {
            continue;
        }
        seen.push(r.nsu_id);
        let regions = db.nsu_regions(r.nsu_id)?.join(",");
        line(&format!("    NSU ID      : {}", r.nsu_id));
        line(&format!("    CDN Regions : {regions}"));
        line(&format!(
            "    Region      : {}",
            eshop_region(&regions.to_uppercase())
        ));
    }

    let updates = db.updates(&query)?;
    if !updates.is_empty() {
        line("  Updates");
        let width = updates.iter().map(|u| u.0.len() + 1).max().unwrap_or(0);
        for (version, date) in &updates {
            line(&format!("    {:<width$} : {date}", format!("v{version}")));
        }
    }

    let mut related: Vec<String> = Vec::new();
    for r in db.related_titles(&query)? {
        if !related.contains(&r) {
            related.push(r);
        }
    }
    if related.len() > 1 {
        line("  Related Titles");
        for r in &related {
            line(&format!("    {}", remove_line_endings(r)));
        }
    }
    line("");
    Ok(())
}

/// Region of one eShop entry from its upper-cased `CDN Regions` list. This
/// is not the classifier used for file names: there is no Latin America and
/// no World, and a later match replaces an earlier one.
fn eshop_region(list: &str) -> &'static str {
    let mut region = "UNKNOWN";
    for (set, name) in [(AMERICAS, "Americas"), (EUROPE, "Europe"), (ASIA, "Asia")] {
        if set.iter().any(|p| list.contains(p)) {
            region = name;
        }
    }
    match list {
        "KR.KO" => "Korea",
        "JP.JA" => "Japan",
        "HK.ZH" => "China",
        _ => region,
    }
}

#[cfg(test)]
mod tests {
    use super::eshop_region;

    #[test]
    fn eshop_region_buckets() {
        assert_eq!(eshop_region(""), "UNKNOWN");
        assert_eq!(eshop_region("US.EN"), "Americas");
        assert_eq!(eshop_region("BR.PT"), "UNKNOWN");
        assert_eq!(eshop_region("US.EN,GB.EN"), "Europe");
        assert_eq!(eshop_region("GB.EN,JP.JA"), "Asia");
        assert_eq!(eshop_region("DE.DE"), "Europe");
        assert_eq!(eshop_region("JP.JA"), "Japan");
        assert_eq!(eshop_region("KR.KO"), "Korea");
        assert_eq!(eshop_region("HK.ZH"), "China");
        assert_eq!(eshop_region("HK.ZH,KR.KO"), "Asia");
    }
}
