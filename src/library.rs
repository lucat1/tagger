use crate::models::{Artist, Release, Track};
use crate::track::format::Format;
use crate::util::path_to_str;
use crate::{DB, SETTINGS};
use async_trait::async_trait;
use eyre::{eyre, Result, WrapErr};
use itertools::Itertools;
use serde_json;
use sqlx::sqlite::SqliteRow;
use sqlx::{Encode, Pool, QueryBuilder, Row, Sqlite, Type};
use std::fmt::Display;
use std::iter;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

pub trait LibraryRelease {
    fn paths(&self) -> Result<Vec<PathBuf>>;
    fn path(&self) -> Result<PathBuf>;
    fn other_paths(&self) -> Result<Vec<PathBuf>>;
}

impl LibraryRelease for Release {
    fn paths(&self) -> Result<Vec<PathBuf>> {
        let mut v = vec![];
        let settings = SETTINGS
            .get()
            .ok_or(eyre!("Could not read settings"))
            .wrap_err("While generating a path for the library")?;
        for artist in self.artists.iter() {
            let path_str = settings
                .release_name
                .replace("{release.artist}", artist.name.as_str())
                .replace("{release.title}", self.title.as_str());
            v.push(settings.library.join(PathBuf::from(path_str)))
        }
        Ok(v)
    }

    fn path(&self) -> Result<PathBuf> {
        self.paths()?
            .first()
            .map_or(
                Err(eyre!("Release does not have a path in the library, most definitely because the release has no artists")),
                |p| Ok(p.clone())
            )
    }

    fn other_paths(&self) -> Result<Vec<PathBuf>> {
        let main = self.path()?;
        Ok(self
            .paths()?
            .iter()
            .filter_map(|p| -> Option<PathBuf> {
                if *p != main {
                    Some(p.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>())
    }
}

pub trait LibraryTrack {
    fn path(&self) -> Result<PathBuf>;
}

impl LibraryTrack for Track {
    fn path(&self) -> Result<PathBuf> {
        let base = self
            .release
            .clone()
            .ok_or(eyre!("This track doesn't belong to any release"))?
            .path()?;
        let settings = SETTINGS
            .get()
            .ok_or(eyre!("Could not read settings"))
            .wrap_err("While generating a path for the library")?;
        let mut extensionless = settings.track_name.clone();
        extensionless.push('.');
        extensionless.push_str(
            self.format
                .ok_or(eyre!("The given Track doesn't have an associated format"))?
                .ext(),
        );
        let path_str = extensionless
            .replace(
                "{track.disc}",
                self.disc
                    .ok_or(eyre!("The track has no disc"))?
                    .to_string()
                    .as_str(),
            )
            .replace(
                "{track.number}",
                self.number
                    .ok_or(eyre!("The track has no number"))?
                    .to_string()
                    .as_str(),
            )
            .replace("{track.title}", self.title.as_str());
        Ok(base.join(path_str))
    }
}

pub trait Value<'args>: Encode<'args, Sqlite> + sqlx::Type<Sqlite> {}

#[async_trait]
pub trait InTable {
    fn table() -> &'static str;
    fn fields() -> Vec<&'static str>;
    fn decode(row: SqliteRow) -> Result<Self, sqlx::Error>
    where
        Self: Sized;
    async fn fill_relationships(&mut self, db: &Pool<Sqlite>) -> Result<()>;
}

pub trait Builder: InTable {
    fn query_builder<'args, B, D>(
        fields: Vec<(D, B)>,
        extra: Vec<D>,
    ) -> QueryBuilder<'args, Sqlite>
    where
        B: 'args + Encode<'args, Sqlite> + Send + Type<Sqlite>,
        D: Display;
    fn store_builder<'args>() -> QueryBuilder<'args, Sqlite>;
}

#[async_trait]
pub trait Filter: Builder {
    async fn filter<'args, B, D>(fields: Vec<(D, B)>, extra: Vec<D>) -> Result<Vec<Self>>
    where
        B: 'args + Encode<'args, Sqlite> + Send + Type<Sqlite>,
        D: Display + Send,
        Self: Sized;
}

#[async_trait]
pub trait Fetch: Builder {
    async fn fetch(mbid: String) -> Result<Self>
    where
        Self: Sized;
}

#[async_trait]
pub trait Store: Builder {
    async fn store(&self) -> Result<()>;
}

impl<T> Builder for T
where
    T: InTable,
{
    fn query_builder<'args, B, D>(fields: Vec<(D, B)>, extra: Vec<D>) -> QueryBuilder<'args, Sqlite>
    where
        B: 'args + Encode<'args, Sqlite> + Send + Type<Sqlite>,
        D: Display,
    {
        let mut qb = QueryBuilder::new("SELECT ");
        qb.push(Self::fields().join(","));
        qb.push(" FROM ");
        qb.push(Self::table());
        if fields.len() > 0 {
            qb.push(" WHERE ");
            let len = fields.len();
            for (i, (key, val)) in fields.into_iter().enumerate() {
                qb.push(format!("{} = ", key));
                qb.push_bind(val);
                if i < len - 1 {
                    qb.push(" AND ");
                }
            }
        }
        for ex in extra.into_iter() {
            qb.push(ex);
        }
        qb
    }
    fn store_builder<'args>() -> QueryBuilder<'args, Sqlite> {
        let mut qb = QueryBuilder::new("INSERT OR REPLACE INTO ");
        qb.push(Self::table());
        qb.push(" (");
        qb.push(Self::fields().join(","));
        qb.push(") VALUES (");
        qb.push(iter::repeat("?").take(Self::fields().len()).join(","));
        qb.push(")");
        qb
    }
}

#[async_trait]
impl<T> Filter for T
where
    T: Builder + Send + Unpin,
{
    async fn filter<'args, B, D>(fields: Vec<(D, B)>, extra: Vec<D>) -> Result<Vec<Self>>
    where
        B: 'args + Encode<'args, Sqlite> + Send + Type<Sqlite>,
        D: Display + Send,
        Self: Sized,
    {
        let db = DB.get().ok_or(eyre!("Could not get database"))?;
        let mut vals = Self::query_builder(fields, extra)
            .build()
            .try_map(Self::decode)
            .fetch_all(db)
            .await?;
        for val in vals.iter_mut() {
            val.fill_relationships(db).await?;
        }
        Ok(vals)
    }
}

#[async_trait]
impl<T> Fetch for T
where
    T: Builder + Send + Unpin,
{
    async fn fetch(mbid: String) -> Result<Self>
    where
        Self: Sized,
    {
        let db = DB.get().ok_or(eyre!("Could not get database"))?;
        let mut val = Self::query_builder(vec![("mbid", mbid)], vec!["LIMIT 1"])
            .build()
            .try_map(Self::decode)
            .fetch_one(db)
            .await?;
        val.fill_relationships(db).await?;
        Ok(val)
    }
}

#[async_trait]
impl InTable for Artist {
    fn table() -> &'static str {
        "artists"
    }
    fn fields() -> Vec<&'static str> {
        vec!["mbid", "name", "sort_name", "instruments"]
    }
    fn decode(row: SqliteRow) -> Result<Self, sqlx::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            mbid: row.try_get("mbid").ok(),
            name: row.try_get("name")?,
            join_phrase: None,
            sort_name: row.try_get("sort_name").ok(),
            instruments: serde_json::from_str(row.try_get("instruments")?)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        })
    }
    async fn fill_relationships(&mut self, _: &Pool<Sqlite>) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Store for Artist {
    async fn store(&self) -> Result<()> {
        Self::store_builder()
            .build()
            .bind(&self.mbid)
            .bind(&self.name)
            .bind(&self.sort_name)
            .bind(serde_json::to_string(&self.instruments)?)
            .execute(DB.get().ok_or(eyre!("Could not get database"))?)
            .await?;
        Ok(())
    }
}

async fn resolve(db: &Pool<Sqlite>, table: &str, mbid: Option<&String>) -> Result<Vec<Artist>> {
    Artist::query_builder::<String, String>(vec![], vec![])
        .push(format!(
            " WHERE mbid = (SELECT artist FROM {} WHERE ref =",
            table
        ))
        .push_bind(mbid)
        .push(")")
        .build()
        .try_map(Artist::decode)
        .fetch_all(db)
        .await
        .map_err(|e| eyre!(e))
}

async fn link(
    db: &Pool<Sqlite>,
    table: &str,
    mbid: Option<&String>,
    artist: &Artist,
) -> Result<()> {
    sqlx::query(
        format!(
            "INSERT OR REPLACE INTO {} (ref, artist) VALUES (?, ?)",
            table
        )
        .as_str(),
    )
    .bind(mbid)
    .bind(artist.mbid.as_ref())
    .execute(db)
    .await?;
    Ok(())
}

#[async_trait]
impl InTable for Track {
    fn table() -> &'static str {
        "tracks"
    }
    fn fields() -> Vec<&'static str> {
        vec![
            "mbid",
            "title",
            "length",
            "disc",
            "disc_mbid",
            "number",
            "genres",
            "release",
            "format",
            "path",
        ]
    }
    fn decode(row: SqliteRow) -> Result<Self, sqlx::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            mbid: row.try_get("mbid").ok(),
            title: row.try_get("title")?,
            artists: vec![],
            length: row
                .try_get("title")
                .ok()
                .map(|d: i64| Duration::from_secs(d as u64)),
            disc: row.try_get("disc").ok().map(|d: i64| d as u64),
            disc_mbid: row.try_get("disc_mbid").ok(),
            number: row.try_get("number").ok().map(|d: i64| d as u64),
            genres: serde_json::from_str(row.try_get("genres")?)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            release: None,

            performers: vec![],
            engigneers: vec![],
            mixers: vec![],
            producers: vec![],
            lyricists: vec![],
            writers: vec![],
            composers: vec![],

            format: row
                .try_get("format")
                .map_or(Ok(None), |f| Format::from_ext(f).map(|s| Some(s)))
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            path: row
                .try_get("path")
                .map_or(None, |p: &str| PathBuf::from_str(p).ok()),
        })
    }
    async fn fill_relationships(&mut self, db: &Pool<Sqlite>) -> Result<()> {
        self.artists = resolve(db, "track_artists", self.mbid.as_ref()).await?;
        self.performers = resolve(db, "track_performers", self.mbid.as_ref()).await?;
        self.engigneers = resolve(db, "track_engigneers", self.mbid.as_ref()).await?;
        self.mixers = resolve(db, "track_mixers", self.mbid.as_ref()).await?;
        self.producers = resolve(db, "track_producers", self.mbid.as_ref()).await?;
        self.lyricists = resolve(db, "track_lyricists", self.mbid.as_ref()).await?;
        self.writers = resolve(db, "track_writers", self.mbid.as_ref()).await?;
        self.composers = resolve(db, "track_composers", self.mbid.as_ref()).await?;
        Ok(())
    }
}

#[async_trait]
impl Store for Track {
    async fn store(&self) -> Result<()> {
        if let Some(rel) = &self.release {
            rel.store().await?;
        }
        let db = DB.get().ok_or(eyre!("Could not get database"))?;
        Track::store_builder()
            .build()
            .bind(&self.mbid)
            .bind(&self.title)
            .bind(&self.length.map(|t| t.as_secs() as i64))
            .bind(&self.disc.map(|n| n as i64))
            .bind(&self.disc_mbid)
            .bind(&self.number.map(|n| n as i64))
            .bind(serde_json::to_string(&self.genres)?)
            .bind(self.release.as_ref().map_or(None, |r| r.mbid.as_ref()))
            .bind(self.format.map(|f| String::from(f)))
            .bind(self.path.as_ref().map_or(
                Err(eyre!("The given track doesn't have an associated path")),
                |p| path_to_str(p),
            )?)
            .execute(db)
            .await?;

        for artist in self.artists.iter() {
            artist.store().await?;
            link(db, "track_artists", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.performers.iter() {
            artist.store().await?;
            link(db, "track_performers", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.engigneers.iter() {
            artist.store().await?;
            link(db, "track_engigneers", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.mixers.iter() {
            artist.store().await?;
            link(db, "track_mixers", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.producers.iter() {
            artist.store().await?;
            link(db, "track_producers", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.lyricists.iter() {
            artist.store().await?;
            link(db, "track_lyricists", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.writers.iter() {
            artist.store().await?;
            link(db, "track_writers", self.mbid.as_ref(), artist).await?;
        }
        for artist in self.composers.iter() {
            artist.store().await?;
            link(db, "track_composers", self.mbid.as_ref(), artist).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl InTable for Release {
    fn table() -> &'static str {
        "releases"
    }
    fn fields() -> Vec<&'static str> {
        vec![
            "mbid",
            "release_group_mbid",
            "asin",
            "title",
            "discs",
            "media",
            "tracks",
            "country",
            "label",
            "catalog_no",
            "status",
            "release_type",
            "date",
            "original_date",
            "script",
        ]
    }
    fn decode(row: SqliteRow) -> Result<Self, sqlx::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            mbid: row.try_get("mbid").ok(),
            release_group_mbid: row.try_get("release_group_mbid").ok(),
            asin: row.try_get("asin").ok(),
            title: row.try_get("title")?,
            artists: vec![],
            discs: row.try_get("discs").ok().map(|d: i64| d as u64),
            media: row.try_get("media").ok(),
            tracks: row.try_get("tracks").ok().map(|d: i64| d as u64),
            country: row.try_get("country").ok(),
            label: row.try_get("label").ok(),
            catalog_no: row.try_get("catalog_no").ok(),
            status: row.try_get("status").ok(),
            release_type: row.try_get("release_type").ok(),
            date: row.try_get("date").ok(),
            original_date: row.try_get("original_date").ok(),
            script: row.try_get("script").ok(),
        })
    }
    async fn fill_relationships(&mut self, db: &Pool<Sqlite>) -> Result<()> {
        self.artists = resolve(db, "release_artists", self.mbid.as_ref()).await?;
        Ok(())
    }
}

#[async_trait]
impl Store for Release {
    async fn store(&self) -> Result<()> {
        let db = DB.get().ok_or(eyre!("Could not get database"))?;
        Release::store_builder()
            .build()
            .bind(&self.mbid)
            .bind(&self.release_group_mbid)
            .bind(&self.asin)
            .bind(&self.title)
            .bind(&self.discs.map(|n| n as i64))
            .bind(&self.tracks.map(|n| n as i64))
            .bind(&self.country)
            .bind(&self.label)
            .bind(&self.catalog_no)
            .bind(&self.status)
            .bind(&self.release_type)
            .bind(&self.date)
            .bind(&self.original_date)
            .bind(&self.script)
            .execute(db)
            .await?;
        for artist in self.artists.iter() {
            artist.store().await?;
            link(db, "release_artists", self.mbid.as_ref(), artist).await?;
        }
        Ok(())
    }
}
