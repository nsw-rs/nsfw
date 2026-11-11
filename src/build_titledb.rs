// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `build-titledb`: port of `BuildTitleDbCommand`. Builds `titledb.db` from
//! the blawar/titledb JSON files with the same schema and row order as the
//! original tool. `CnmtData` has no `Id` column: the released binary does
//! not create it, although the class declares one.

use crate::log::{line, olive, part, tag};
use crate::with_suffix;
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use rusqlite::{Connection, params};
use serde::Deserialize;
use std::path::Path;
use std::time::Instant;

const SCHEMA: &str = r#"
create table if not exists "GameInfo" (
"GameId" integer primary key autoincrement not null ,
"NsuId" integer ,
"BannerUrl" varchar ,
"Category" varchar ,
"Description" varchar ,
"Developer" varchar ,
"FrontBoxArt" varchar ,
"IconUrl" varchar ,
"Id" varchar ,
"Ids" varchar ,
"Intro" varchar ,
"IsDemo" integer ,
"Key" varchar ,
"Languages" varchar ,
"Name" varchar ,
"NumberOfPlayers" integer ,
"Publisher" varchar ,
"Rating" integer ,
"RatingContent" varchar ,
"Region" varchar ,
"ReleaseDate" integer ,
"RightsId" varchar ,
"Screenshots" varchar ,
"Size" integer ,
"Version" varchar ,
"RegionLanguage" varchar );
create index if not exists "GameInfo_NsuId" on "GameInfo"("NsuId");
create index if not exists "GameInfo_Id" on "GameInfo"("Id");
create index if not exists "GameInfo_Ids" on "GameInfo"("Ids");
create index if not exists "GameInfo_Name" on "GameInfo"("Name");
create table if not exists "TitleRegion" (
"NsuId" integer ,
"Region" varchar );
create index if not exists "TitleRegion_NsuId" on "TitleRegion"("NsuId");
create index if not exists "TitleRegion_Region" on "TitleRegion"("Region");
create table if not exists "TitleVersions" (
"TitleId" varchar ,
"Version" varchar ,
"ReleaseDate" varchar );
create index if not exists "TitleVersions_TitleId" on "TitleVersions"("TitleId");
create index if not exists "TitleVersions_Version" on "TitleVersions"("Version");
create table if not exists "CnmtData" (
"TitleId" varchar ,
"Version" varchar ,
"NcaId" varchar ,
"NcaType" integer );
create index if not exists "CnmtData_TitleId" on "CnmtData"("TitleId");
"#;

/// The original reads names case-insensitively; blawar uses camelCase.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CnmtJson {
    #[serde(default)]
    content_entries: Vec<CnmtContentJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CnmtContentJson {
    #[serde(default = "empty_string")]
    nca_id: Option<String>,
    #[serde(rename = "type", default)]
    kind: i32,
}

/// Missing fields keep the original's property initializers (`string.Empty`),
/// while an explicit null is stored as NULL.
fn empty_string() -> Option<String> {
    Some(String::new())
}

fn empty_list() -> Option<Vec<Option<String>>> {
    Some(Vec::new())
}

/// Array fields are stored joined with ',' (null elements become empty).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameJson {
    #[serde(default)]
    nsu_id: i64,
    banner_url: Option<String>,
    #[serde(default = "empty_list")]
    category: Option<Vec<Option<String>>>,
    description: Option<String>,
    developer: Option<String>,
    front_box_art: Option<String>,
    icon_url: Option<String>,
    id: Option<String>,
    ids: Option<Vec<Option<String>>>,
    intro: Option<String>,
    #[serde(default)]
    is_demo: bool,
    key: Option<String>,
    languages: Option<Vec<Option<String>>>,
    name: Option<String>,
    number_of_players: Option<i32>,
    publisher: Option<String>,
    rating: Option<i32>,
    rating_content: Option<Vec<Option<String>>>,
    release_date: Option<i32>,
    rights_id: Option<String>,
    screenshots: Option<Vec<Option<String>>>,
    size: Option<i64>,
    version: Option<String>,
}

fn join(v: &Option<Vec<Option<String>>>) -> Option<String> {
    v.as_ref().map(|v| {
        v.iter()
            .map(|s| s.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&data).with_context(|| format!("cannot parse {}", path.display()))
}

/// .NET `Directory.EnumerateFiles(dir, "??.??.json")`, in directory order.
fn region_files(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let b = name.as_bytes();
        if b.len() == 10
            && b[2] == b'.'
            && b[5] == b'.'
            && name.ends_with(".json")
            && entry.path().is_file()
        {
            out.push(dir.join(&name).to_string_lossy().into_owned());
        }
    }
    Ok(out)
}

fn remove_if_there(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn run(dir: &Path, clean: bool) -> Result<()> {
    let db_path = dir.join("titledb.db");
    if clean {
        clean_build(dir, &db_path)?;
    } else {
        build(dir, &db_path)?;
    }
    line(&tag("COMPLETE!"));
    Ok(())
}

/// A clean build goes to a file of its own, which takes the place of the
/// old DB once it is complete: a JSON file that fails to parse leaves the
/// old DB as it was.
fn clean_build(dir: &Path, db_path: &Path) -> Result<()> {
    if db_path.exists() {
        line(&format!("{} Clean old DB.", tag("DONE")));
    }
    let new_path = with_suffix(db_path, ".new");
    let remove_new = || {
        ["", "-wal", "-shm"]
            .iter()
            .try_for_each(|suffix| remove_if_there(&with_suffix(&new_path, suffix)))
    };
    // What a killed run left behind.
    remove_new()?;
    if let Err(e) = build(dir, &new_path) {
        let _ = remove_new();
        return Err(e);
    }
    // The WAL of the old DB must not meet the new one.
    for suffix in ["-wal", "-shm"] {
        remove_if_there(&with_suffix(db_path, suffix))?;
    }
    std::fs::rename(&new_path, db_path)?;
    Ok(())
}

/// Add the rows of the JSON files in `dir` to the DB at `db_path`.
fn build(dir: &Path, db_path: &Path) -> Result<()> {
    let mut conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(SCHEMA)?;
    // One transaction instead of one per row: same rows, much faster.
    let tx = conn.transaction()?;
    // Without `-c` the rows of an earlier run stay. `GameInfo` skips the
    // ones it has; these tables would get every row a second time.
    tx.execute_batch(
        r#"delete from "CnmtData"; delete from "TitleVersions"; delete from "TitleRegion";"#,
    )?;

    part(&format!("Ingesting: {}...", olive("CNMT")));
    let cnmts: IndexMap<String, IndexMap<String, CnmtJson>> = read_json(&dir.join("cnmts.json"))?;
    {
        let mut stmt = tx.prepare(
            r#"insert into "CnmtData"("TitleId","Version","NcaId","NcaType") values (?,?,?,?)"#,
        )?;
        for (title_id, versions) in &cnmts {
            for (version, info) in versions {
                for c in &info.content_entries {
                    stmt.execute(params![title_id, version, c.nca_id, c.kind])?;
                }
            }
        }
    }
    drop(cnmts);
    line(&tag("DONE"));

    part(&format!("Ingesting: {}...", olive("VERSIONS")));
    let versions: IndexMap<String, IndexMap<String, Option<String>>> =
        read_json(&dir.join("versions.json"))?;
    {
        let mut stmt = tx.prepare(
            r#"insert into "TitleVersions"("TitleId","Version","ReleaseDate") values (?,?,?)"#,
        )?;
        for (title_id, entries) in &versions {
            for (version, date) in entries {
                stmt.execute(params![title_id, version, date])?;
            }
        }
    }
    drop(versions);
    line(&tag("DONE"));

    let mut entries: Vec<String> = [
        "US.en", "GB.en", "JP.ja", "DE.de", "FR.fr", "ES.es", "IT.it", "NL.nl", "PT.pt", "KR.ko",
        "RU.ru",
    ]
    .iter()
    .map(|r| dir.join(format!("{r}.json")).to_string_lossy().into_owned())
    .collect();
    for f in region_files(dir)? {
        if !entries.contains(&f) {
            entries.push(f);
        }
    }

    let mut region_stmt =
        tx.prepare(r#"insert into "TitleRegion"("NsuId","Region") values (?,?)"#)?;
    let mut exists_stmt = tx.prepare(
        r#"select count(*) from "GameInfo" where (("NsuId" = (?)) and ("Name" is (?)))"#,
    )?;
    let mut game_stmt = tx.prepare(
        r#"insert into "GameInfo"("NsuId","BannerUrl","Category","Description","Developer","FrontBoxArt","IconUrl","Id","Ids","Intro","IsDemo","Key","Languages","Name","NumberOfPlayers","Publisher","Rating","RatingContent","Region","ReleaseDate","RightsId","Screenshots","Size","Version","RegionLanguage") values (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
    )?;
    for entry in &entries {
        part(&format!("Ingesting: {}...", olive(entry)));
        let timer = Instant::now();
        let path = Path::new(entry);
        let games: IndexMap<String, GameJson> = read_json(path)?;
        let region = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".json", "");
        let Some(region_language) = region.split('.').nth(1) else {
            bail!("Unexpected TitleDB file name {entry}");
        };
        let mut count = 0;
        for g in games.values() {
            region_stmt.execute(params![g.nsu_id, region])?;
            let n: i64 = exists_stmt.query_row(params![g.nsu_id, g.name], |r| r.get(0))?;
            if n != 0 {
                continue;
            }
            game_stmt.execute(params![
                g.nsu_id,
                g.banner_url,
                join(&g.category),
                g.description,
                g.developer,
                g.front_box_art,
                g.icon_url,
                g.id,
                join(&g.ids),
                g.intro,
                g.is_demo,
                g.key,
                join(&g.languages),
                g.name,
                g.number_of_players,
                g.publisher,
                g.rating,
                join(&g.rating_content),
                region,
                g.release_date,
                g.rights_id,
                join(&g.screenshots),
                g.size,
                g.version,
                region_language,
            ])?;
            count += 1;
        }
        line(&format!(
            "{} - Added {count} - ({:.2}s)",
            tag("DONE"),
            timer.elapsed().as_secs_f64()
        ));
    }
    drop((region_stmt, exists_stmt, game_stmt));
    tx.commit()?;
    // Closing folds the WAL into the file.
    conn.close().map_err(|(_, e)| e)?;
    Ok(())
}
