use bathbot_psql::model::osu::ArtistTitle;
use eyre::{Report, Result};

use crate::{
    core::Context,
    util::{gestalt_pattern_matching, levenshtein_similarity},
};

pub struct GameMapset {
    pub mapset_id: u32,
    artist: String,
    title: String,
    title_adjusted: Option<String>,
}

impl GameMapset {
    pub async fn new(ctx: &Context, mapset_id: u32) -> Result<Self> {
        let ArtistTitle { artist, title } = match ctx.osu_map().artist_title(mapset_id).await {
            Ok(mut artist_title) => {
                artist_title.title.make_ascii_lowercase();
                artist_title.artist.make_ascii_lowercase();

                artist_title
            }
            Err(err) => return Err(Report::new(err).wrap_err("failed to get artist and title")),
        };

        let title_adjusted = if let (Some(open), Some(close)) = (title.find('('), title.rfind(')'))
        {
            let mut title_ = title.clone();
            title_.replace_range(open..=close, "");

            if let Some(idx) = title_.find("feat.").or_else(|| title_.find("ft.")) {
                title_.truncate(idx);
            }

            let trimmed = title_.trim();

            if trimmed.len() < title_.len() {
                Some(trimmed.to_owned())
            } else {
                Some(title_)
            }
        } else {
            title
                .find("feat.")
                .or_else(|| title.find("ft."))
                .map(|idx| title[..idx].trim_end().to_owned())
        };

        let mapset = Self {
            artist,
            mapset_id,
            title,
            title_adjusted,
        };

        Ok(mapset)
    }

    pub fn title(&self) -> &str {
        match self.title_adjusted.as_deref() {
            Some(title) => title,
            None => &self.title,
        }
    }

    pub fn artist(&self) -> &str {
        &self.artist
    }

    pub fn matches_title(&self, content: &str, difficulty: f32) -> Option<bool> {
        self.title_adjusted
            .as_deref()
            .and_then(|title| Self::matches(title, content, difficulty))
            .or_else(|| Self::matches(&self.title, content, difficulty))
    }

    pub fn matches_artist(&self, content: &str, difficulty: f32) -> Option<bool> {
        Self::matches(&self.artist, content, difficulty)
    }

    fn matches(src: &str, content: &str, difficulty: f32) -> Option<bool> {
        if src == content {
            Some(true)
        } else if levenshtein_similarity(src, content) > difficulty
            || gestalt_pattern_matching(src, content) > difficulty + 0.1
        {
            Some(false)
        } else {
            None
        }
    }
}
