use std::{error::Error as StdError, fmt};

use futures::{
    future::{BoxFuture, FutureExt},
    stream::{StreamExt, TryStreamExt},
};
use hashbrown::HashMap;
use rosu_v2::prelude::{
    Beatmap, Beatmapset, GameMode,
    RankStatus::{Approved, Loved, Ranked},
};
use sqlx::{Error as SqlxError, PgConnection};
use thiserror::Error;

use crate::{
    database::{DBBeatmap, DBBeatmapset},
    BotResult, Database,
};

macro_rules! invalid_status {
    ($obj:ident) => {
        !matches!($obj.status, Ranked | Loved | Approved)
    };
}

type InsertMapResult<T> = Result<T, InsertMapOrMapsetError>;

#[derive(Debug)]
pub enum InsertMapOrMapsetError {
    Map(InsertMapError),
    Mapset(InsertMapsetError),
    Sqlx(SqlxError),
}

impl From<InsertMapError> for InsertMapOrMapsetError {
    fn from(err: InsertMapError) -> Self {
        Self::Map(err)
    }
}

impl From<InsertMapsetError> for InsertMapOrMapsetError {
    fn from(err: InsertMapsetError) -> Self {
        Self::Mapset(err)
    }
}

impl From<SqlxError> for InsertMapOrMapsetError {
    fn from(err: SqlxError) -> Self {
        Self::Sqlx(err)
    }
}

impl StdError for InsertMapOrMapsetError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Map(err) => err.source(),
            Self::Mapset(err) => err.source(),
            Self::Sqlx(err) => Some(err),
        }
    }
}

impl fmt::Display for InsertMapOrMapsetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map(err) => write!(f, "{err}"),
            Self::Mapset(err) => write!(f, "{err}"),
            Self::Sqlx(_) => f.write_str("sqlx error"),
        }
    }
}

#[derive(Debug, Error)]
pub enum InsertMapError {
    #[error("cannot add {0:?} map to DB without combo")]
    MissingCombo(GameMode),
    #[error("failed to add map to DB")]
    Sqlx(#[from] SqlxError),
}

#[derive(Debug, Error)]
pub enum InsertMapsetError {
    #[error("cannot add mapset to DB without genre")]
    MissingGenre,
    #[error("cannot add mapset to DB without language")]
    MissingLanguage,
    #[error("failed to add mapset to DB")]
    Sqlx(#[from] SqlxError),
}

fn should_not_be_stored(map: &Beatmap) -> bool {
    invalid_status!(map) || map.convert || (map.mode != GameMode::MNA && map.max_combo.is_none())
}

impl Database {
    pub async fn get_beatmap(&self, map_id: u32, with_mapset: bool) -> BotResult<Beatmap> {
        let mut conn = self.pool.acquire().await?;

        let query = sqlx::query_as!(
            DBBeatmap,
            "SELECT * FROM maps WHERE map_id=$1",
            map_id as i32
        );

        let row = query.fetch_one(&mut conn).await?;
        let mut map = Beatmap::from(row);

        if with_mapset {
            let query = sqlx::query_as!(
                DBBeatmapset,
                "SELECT * FROM mapsets WHERE mapset_id=$1",
                map.mapset_id as i32
            );

            let mapset = query.fetch_one(&mut conn).await?;

            map.mapset.replace(mapset.into());
        }

        Ok(map)
    }

    pub async fn get_beatmapset<T: From<DBBeatmapset>>(&self, mapset_id: u32) -> BotResult<T> {
        let query = sqlx::query_as!(
            DBBeatmapset,
            "SELECT * FROM mapsets WHERE mapset_id=$1",
            mapset_id as i32
        );

        let row = query.fetch_one(&self.pool).await?;

        Ok(row.into())
    }

    pub async fn get_beatmap_combo(&self, map_id: u32) -> BotResult<Option<u32>> {
        let row = sqlx::query!("SELECT max_combo FROM maps WHERE map_id=$1", map_id as i32)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.max_combo.map(|c| c as u32))
    }

    pub async fn get_beatmaps_combo(
        &self,
        map_ids: &[i32],
    ) -> BotResult<HashMap<u32, Option<u32>>> {
        let mut combos = HashMap::with_capacity(map_ids.len());

        let query = sqlx::query!(
            "SELECT map_id,max_combo FROM maps WHERE map_id=ANY($1)",
            map_ids
        );

        let mut rows = query.fetch(&self.pool);

        while let Some(row) = rows.next().await.transpose()? {
            combos.insert(row.map_id as u32, row.max_combo.map(|c| c as u32));
        }

        Ok(combos)
    }

    pub async fn get_beatmaps(
        &self,
        map_ids: &[i32],
        with_mapset: bool,
    ) -> BotResult<HashMap<u32, Beatmap>> {
        if map_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut conn = self.pool.acquire().await?;

        let query = sqlx::query_as!(
            DBBeatmap,
            "SELECT * FROM maps WHERE map_id=ANY($1)",
            map_ids
        );

        let mut stream = query
            .fetch(&mut conn)
            .map_ok(Beatmap::from)
            .map_ok(|m| (m.map_id, m));

        let mut beatmaps = HashMap::with_capacity(map_ids.len());

        while let Some((id, mut map)) = stream.next().await.transpose()? {
            if with_mapset {
                let query = sqlx::query_as!(
                    DBBeatmapset,
                    "SELECT * FROM mapsets WHERE mapset_id=$1",
                    map.mapset_id as i32
                );

                let mapset = query.fetch_one(&self.pool).await?;
                map.mapset.replace(mapset.into());
            }

            beatmaps.insert(id, map);
        }

        Ok(beatmaps)
    }

    pub async fn insert_beatmapset(&self, mapset: &Beatmapset) -> InsertMapResult<bool> {
        if invalid_status!(mapset) {
            return Ok(false);
        }

        let mut conn = self.pool.acquire().await?;

        insert_mapset_(&mut conn, mapset).await.map(|_| true)
    }

    pub async fn insert_beatmap(&self, map: &Beatmap) -> InsertMapResult<bool> {
        if should_not_be_stored(map) {
            return Ok(false);
        }

        let mut conn = self.pool.acquire().await?;

        insert_map_(&mut conn, map).await.map(|_| true)
    }

    pub async fn insert_beatmaps(
        &self,
        maps: impl Iterator<Item = &Beatmap>,
    ) -> InsertMapResult<usize> {
        let mut conn = self.pool.acquire().await?;
        let mut count = 0;

        for map in maps {
            if should_not_be_stored(map) {
                continue;
            }

            insert_map_(&mut conn, map).await?;
            count += 1;
        }

        Ok(count)
    }
}

async fn insert_map_(conn: &mut PgConnection, map: &Beatmap) -> InsertMapResult<()> {
    let max_combo = if map.mode == GameMode::MNA {
        None
    } else if let Some(combo) = map.max_combo {
        Some(combo as i32)
    } else {
        return Err(InsertMapError::MissingCombo(map.mode).into());
    };

    let query = sqlx::query!(
        "INSERT INTO maps (\
            map_id,\
            mapset_id,\
            checksum,\
            version,\
            seconds_total,\
            seconds_drain,\
            count_circles,\
            count_sliders,\
            count_spinners,\
            hp,\
            cs,\
            od,\
            ar,\
            mode,\
            status,\
            last_update,\
            stars,\
            bpm,\
            max_combo\
        )\
        VALUES\
        ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)\
        ON CONFLICT (map_id) DO NOTHING",
        map.map_id as i32,
        map.mapset_id as i32,
        map.checksum,
        map.version,
        map.seconds_total as i32,
        map.seconds_drain as i32,
        map.count_circles as i32,
        map.count_sliders as i32,
        map.count_spinners as i32,
        map.hp,
        map.cs,
        map.od,
        map.ar,
        map.mode as i16,
        map.status as i16,
        map.last_updated,
        map.stars,
        map.bpm,
        max_combo,
    );

    query
        .execute(&mut *conn)
        .await
        .map_err(InsertMapError::from)?;

    if let Some(ref mapset) = map.mapset {
        insert_mapset_(conn, mapset).await?;
    }

    Ok(())
}

fn insert_mapset_<'a>(
    conn: &'a mut PgConnection,
    mapset: &'a Beatmapset,
) -> BoxFuture<'a, InsertMapResult<()>> {
    let fut = async move {
        let genre = if let Some(genre) = mapset.genre {
            Some(genre as i16)
        } else {
            return Err(InsertMapsetError::MissingGenre.into());
        };

        let language = if let Some(language) = mapset.language {
            Some(language as i16)
        } else {
            return Err(InsertMapsetError::MissingLanguage.into());
        };

        let query = sqlx::query!(
            "INSERT INTO mapsets (\
                mapset_id,\
                user_id,\
                artist,\
                title,\
                creator,\
                status,\
                ranked_date,\
                genre,\
                language,\
                bpm\
            )\
            VALUES\
            ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)\
            ON CONFLICT (mapset_id) DO NOTHING",
            mapset.mapset_id as i32,
            mapset.creator_id as i32,
            mapset.artist,
            mapset.title,
            mapset.creator_name.as_str(),
            mapset.status as i16,
            mapset.ranked_date,
            genre,
            language,
            mapset.bpm,
        );

        query
            .execute(&mut *conn)
            .await
            .map_err(InsertMapsetError::from)?;

        if let Some(ref maps) = mapset.maps {
            for map in maps {
                insert_map_(conn, map).await?;
            }
        }

        Ok(())
    };

    fut.boxed()
}
