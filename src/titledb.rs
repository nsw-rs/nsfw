// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TitleDB lookups: port of the `GetTitleDbInfo`, `LookUpTitle`,
//! `LookUpReleaseDate`, `LookupLanguages`, `LookUpRegions`, `LookUpUpdates`
//! and `LookUpRelatedTitles` helpers from `NsfwUtilities`. The queries match
//! the SQL the original ORM emits (no ORDER BY), so rows come back in the
//! same order before the stable sorts.

use anyhow::{Result, bail};
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

use crate::naming::{clean_title, remove_brackets, remove_line_endings};

pub struct TitleDb {
    conn: Connection,
}

pub struct GameRow {
    pub name: Option<String>,
    pub release_date: Option<i64>,
    pub region_language: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    /// A NULL reads as 0, like the original's `long` property.
    pub nsu_id: i64,
}

const GAME_COLUMNS: &str =
    "\"Name\", \"ReleaseDate\", \"RegionLanguage\", \"Publisher\", \"Description\", \"NsuId\"";

/// Position in `order`, -1 when missing (.NET `List.IndexOf`).
fn index_of(order: &[&str], s: Option<&str>) -> i32 {
    s.and_then(|s| order.iter().position(|o| *o == s))
        .map_or(-1, |p| p as i32)
}

/// SQLite URI that opens `path` as a file nobody changes.
fn immutable_uri(path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut uri = String::from("file:");
    // A drive path needs a root in front, and `/` between its parts.
    if cfg!(windows) {
        uri.push('/');
    }
    for &b in path.as_bytes() {
        match b {
            b'\\' if cfg!(windows) => uri.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'-' | b'_' | b':' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri + "?immutable=1"
}

impl TitleDb {
    pub fn open(path: &Path) -> Result<Self> {
        // The DB is in WAL mode, and a plain read-only open of one writes
        // `-shm` and `-wal` files next to it and fails on a read-only
        // medium. An immutable open reads the file as it is, so it must
        // not be used while a WAL holds changes. It is also what a path
        // that makes no valid URI (a network share on Windows) falls back
        // from.
        let wal = crate::with_suffix(path, "-wal");
        let wal_in_use = std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0);
        let immutable = if wal_in_use {
            None
        } else {
            Connection::open_with_flags(
                immutable_uri(path),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
            .ok()
        };
        let conn = match immutable {
            Some(c) => c,
            None => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        };
        // The file comes from somewhere else: what its schema says (views,
        // triggers, functions in them) gets no trust.
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        Ok(Self { conn })
    }

    fn game_rows(&self, sql: &str, title_id: &str) -> Result<Vec<GameRow>> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params![title_id], |r| {
            Ok(GameRow {
                name: r.get(0)?,
                release_date: r.get(1)?,
                region_language: r.get(2)?,
                publisher: r.get(3)?,
                description: r.get(4)?,
                nsu_id: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Rows for `title_id`, falling back to a substring match on `Ids`,
    /// sorted by preferred language.
    pub fn game_info(&self, title_id: &str) -> Result<Vec<GameRow>> {
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
                &format!("select {GAME_COLUMNS} from \"GameInfo\" where (\"Ids\" like ('%' || (?) || '%'))"),
                title_id,
            )?
        } else {
            self.game_rows(
                &format!("select {GAME_COLUMNS} from \"GameInfo\" where (\"Id\" = (?))"),
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
        let mut regions: Vec<String> = Vec::new();
        for nsu_id in nsu_ids {
            for r in self.nsu_regions(nsu_id)? {
                let r = r.to_uppercase();
                if !regions.contains(&r) {
                    regions.push(r);
                }
            }
        }
        Ok(distribution_region(&regions.join(",")).to_string())
    }

    /// `TitleRegion` rows of one eShop entry, as stored.
    pub fn nsu_regions(&self, nsu_id: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("select \"Region\" from \"TitleRegion\" where (\"NsuId\" = (?))")?;
        let rows = stmt.query_map(params![nsu_id], |r| r.get::<_, Option<String>>(0))?;
        rows.map(|r| Ok(r?.unwrap_or_default())).collect()
    }

    /// Known updates of `title_id` as (version, release date).
    pub fn updates(&self, title_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "select \"Version\", \"ReleaseDate\" from \"TitleVersions\" where (\"TitleId\" = (?))",
        )?;
        let rows = stmt.query_map(params![title_id.to_lowercase()], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Titles whose ID shares all but the last three characters of
    /// `title_id` (base, update, DLCs), as `[Id] Name`. The prefix match is
    /// case-sensitive, like the original's `StartsWith` translation.
    pub fn related_titles(&self, title_id: &str) -> Result<Vec<String>> {
        let n = title_id.chars().count().saturating_sub(3);
        let prefix: String = title_id.chars().take(n).collect();
        let mut stmt = self.conn.prepare_cached(
            "select \"Id\", \"Name\" from \"GameInfo\" where (substr(\"Id\", 1, (?)) = (?))",
        )?;
        let rows = stmt.query_map(params![n as i64, prefix], |r| {
            let id: Option<String> = r.get(0)?;
            let name: Option<String> = r.get(1)?;
            Ok(format!(
                "[{}] {}",
                id.unwrap_or_default(),
                name.map(|n| clean_title(&remove_brackets(&n)))
                    .unwrap_or_default()
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// eShop region prefixes, shared with the `query` region classifier.
pub const AMERICAS: &[&str] = &["US.", "CA.", "MX."];
pub const EUROPE: &[&str] = &[
    "GB.", "DE.", "FR.", "ES.", "IT.", "PT.", "CH.", "HU.", "LT.", "BE.", "BG.", "EE.", "LU.",
    "HR.", "SI.", "AT.", "GR.", "NO.", "DK.", "CZ.", "RO.", "ZA.", "NZ.", "LV.", "SK.", "SE.",
    "FI.", "IE.", "AU.", "MT.", "CY.",
];
pub const ASIA: &[&str] = &["HK.", "KR.", "JP"];

fn distribution_region(list: &str) -> &'static str {
    const LATIN_AMERICA: &[&str] = &["BR.", "AR.", "CL.", "CO.", "CR.", "EC.", "GT.", "PE."];

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
    use super::*;

    #[cfg(unix)]
    #[test]
    fn immutable_uri_escapes() {
        assert_eq!(
            immutable_uri(Path::new("/a b/t?x#y%z/titledb.db")),
            "file:/a%20b/t%3Fx%23y%25z/titledb.db?immutable=1"
        );
        assert_eq!(
            immutable_uri(Path::new("/d/\u{e9}.db")),
            "file:/d/%C3%A9.db?immutable=1"
        );
    }

    /// A read must leave nothing next to a WAL mode DB, and must work when
    /// the directory cannot be written.
    #[cfg(unix)]
    #[test]
    fn open_leaves_no_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/titledb open #1");
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("titledb.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            r#"create table "GameInfo" ("Id" varchar, "Ids" varchar, "Name" varchar,
               "ReleaseDate" integer, "RegionLanguage" varchar, "Publisher" varchar,
               "Description" varchar, "NsuId" integer);
               insert into "GameInfo" ("Id", "Name") values ('0100000000001000', 'A (B)');"#,
        )
        .unwrap();
        drop(conn);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let db = TitleDb::open(&path).unwrap();
        let title = db.look_up_title("0100000000001000").unwrap();
        drop(db);
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(title.as_deref(), Some("A - B"));
        assert_eq!(files, ["titledb.db"]);
    }

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
