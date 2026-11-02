// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TitleDB lookups: port of the `GetTitleDbInfo`, `LookUpTitle`,
//! `LookUpReleaseDate`, `LookupLanguages` and `LookUpRegions` helpers from
//! `NsfwUtilities`. The queries match the SQL the original ORM emits (no
//! ORDER BY), so rows come back in the same order before the stable sorts.

use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

use crate::naming::{clean_title, remove_brackets, remove_line_endings};

pub struct TitleDb {
    conn: Connection,
}

struct GameRow {
    name: Option<String>,
    release_date: Option<i64>,
    region_language: Option<String>,
}

/// Position in `order`, -1 when missing (.NET `List.IndexOf`).
fn index_of(order: &[&str], s: Option<&str>) -> i32 {
    s.and_then(|s| order.iter().position(|o| *o == s))
        .map_or(-1, |p| p as i32)
}

impl TitleDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
    }

    fn game_rows(&self, sql: &str, title_id: &str) -> Result<Vec<GameRow>> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params![title_id], |r| {
            Ok(GameRow {
                name: r.get(0)?,
                release_date: r.get(1)?,
                region_language: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Rows for `title_id`, falling back to a substring match on `Ids`,
    /// sorted by preferred language.
    fn game_info(&self, title_id: &str) -> Result<Vec<GameRow>> {
        const ORDER: &[&str] = &[
            "en", "ja", "de", "fr", "es", "it", "nl", "pt", "ko", "tw", "cn", "zh", "ru",
        ];
        let count: i64 = self.conn.query_row(
            "select count(*) from \"GameInfo\" where (\"Id\" = (?))",
            params![title_id],
            |r| r.get(0),
        )?;
        let mut rows = if count == 0 {
            self.game_rows(
                "select \"Name\", \"ReleaseDate\", \"RegionLanguage\" from \"GameInfo\" where (\"Ids\" like ('%' || (?) || '%'))",
                title_id,
            )?
        } else {
            self.game_rows(
                "select \"Name\", \"ReleaseDate\", \"RegionLanguage\" from \"GameInfo\" where (\"Id\" = (?))",
                title_id,
            )?
        };
        rows.sort_by_key(|r| index_of(ORDER, r.region_language.as_deref()));
        Ok(rows)
    }

    pub fn look_up_title(&self, title_id: &str) -> Result<Option<String>> {
        Ok(self
            .game_info(title_id)?
            .into_iter()
            .next()
            .map(|r| match r.name {
                Some(n) => clean_title(&remove_brackets(&remove_line_endings(&n))),
                None => "UNKNOWN".to_string(),
            }))
    }

    /// Release date as `yyyy-MM-dd`, from a `yyyyMMdd` integer.
    pub fn look_up_release_date(&self, title_id: &str) -> Result<Option<String>> {
        let Some(row) = self.game_info(title_id)?.into_iter().next() else {
            return Ok(None);
        };
        let Some(v) = row
            .release_date
            .filter(|&v| v != 0 && v.to_string().len() == 8)
        else {
            return Ok(None);
        };
        let (y, m, d) = (v / 10000, (v / 100) % 100, v % 100);
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let days = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if y < 1 || d < 1 || d > days {
            bail!("Invalid TitleDB release date {v} for {title_id}");
        }
        Ok(Some(format!("{y:04}-{m:02}-{d:02}")))
    }

    /// Languages of the first row for `title_id`, in preferred order.
    pub fn look_up_languages(&self, title_id: &str) -> Result<Vec<String>> {
        const ORDER: &[&str] = &[
            "en", "ja", "de", "fr", "es", "it", "nl", "pt", "ko", "zh", "ru",
        ];
        let mut stmt = self.conn.prepare_cached(
            "select \"Languages\" from \"GameInfo\" where (\"Id\" = (?)) limit 1",
        )?;
        let mut rows = stmt.query(params![title_id])?;
        let languages: Option<String> = match rows.next()? {
            Some(r) => r.get(0)?,
            None => None,
        };
        let Some(languages) = languages else {
            return Ok(Vec::new());
        };
        let mut v: Vec<String> = languages
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        v.sort_by_key(|s| index_of(ORDER, Some(s)));
        Ok(v)
    }

    /// Distribution region from the eShop regions the title is sold in.
    pub fn look_up_region(&self, title_id: &str) -> Result<String> {
        let mut stmt = self
            .conn
            .prepare_cached("select \"NsuId\" from \"GameInfo\" where (\"Id\" = (?))")?;
        let mut nsu_ids: Vec<i64> = Vec::new();
        for id in stmt.query_map(params![title_id], |r| r.get::<_, Option<i64>>(0))? {
            let id = id?.unwrap_or(0);
            if !nsu_ids.contains(&id) {
                nsu_ids.push(id);
            }
        }
        let mut stmt = self
            .conn
            .prepare_cached("select \"Region\" from \"TitleRegion\" where (\"NsuId\" = (?))")?;
        let mut regions: Vec<String> = Vec::new();
        for nsu_id in nsu_ids {
            for r in stmt.query_map(params![nsu_id], |r| r.get::<_, Option<String>>(0))? {
                let r = r?.unwrap_or_default().to_uppercase();
                if !regions.contains(&r) {
                    regions.push(r);
                }
            }
        }
        Ok(distribution_region(&regions.join(",")).to_string())
    }
}

fn distribution_region(list: &str) -> &'static str {
    const AMERICAS: &[&str] = &["US.", "CA.", "MX."];
    const LATIN_AMERICA: &[&str] = &["BR.", "AR.", "CL.", "CO.", "CR.", "EC.", "GT.", "PE."];
    const EUROPE: &[&str] = &[
        "GB.", "DE.", "FR.", "ES.", "IT.", "PT.", "CH.", "HU.", "LT.", "BE.", "BG.", "EE.", "LU.",
        "HR.", "SI.", "AT.", "GR.", "NO.", "DK.", "CZ.", "RO.", "ZA.", "NZ.", "LV.", "SK.", "SE.",
        "FI.", "IE.", "AU.", "MT.", "CY.",
    ];
    const ASIA: &[&str] = &["HK.", "KR.", "JP"];

    let mut region = "UNKNOWN";
    let mut matches = 0;
    for (set, name) in [
        (LATIN_AMERICA, "Latin America"),
        (AMERICAS, "North America"),
        (EUROPE, "Europe"),
        (ASIA, "Asia"),
    ] {
        if set.iter().any(|p| list.contains(p)) {
            region = name;
            matches += 1;
        }
    }
    region = match list {
        "KR.KO" => "Korea",
        "JP.JA" => "Japan",
        "HK.ZH" => "China",
        "US.EN" => "USA",
        "DE.DE" => "Germany",
        _ => region,
    };
    if matches > 1 {
        region = "World";
    }
    region
}

#[cfg(test)]
mod tests {
    use super::distribution_region;

    #[test]
    fn region_buckets() {
        assert_eq!(distribution_region(""), "UNKNOWN");
        assert_eq!(distribution_region("US.EN"), "USA");
        assert_eq!(distribution_region("US.EN,CA.FR"), "North America");
        assert_eq!(distribution_region("BR.PT"), "Latin America");
        assert_eq!(distribution_region("GB.EN,DE.DE"), "Europe");
        assert_eq!(distribution_region("DE.DE"), "Germany");
        assert_eq!(distribution_region("JP.JA"), "Japan");
        assert_eq!(distribution_region("KR.KO"), "Korea");
        assert_eq!(distribution_region("HK.ZH"), "China");
        assert_eq!(distribution_region("HK.ZH,KR.KO"), "Asia");
        assert_eq!(distribution_region("US.EN,GB.EN"), "World");
        assert_eq!(distribution_region("US.EN,MX.ES,BR.PT"), "World");
    }
}
