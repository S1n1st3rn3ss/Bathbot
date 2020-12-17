use crate::{
    embeds::{osu, Author, EmbedData, Footer},
    util::{
        constants::AVATAR_URL,
        numbers::{with_comma, with_comma_u64},
        osu::pp_missing,
    },
};

use rosu::model::{Score, User};
use twilight_embed_builder::image_source::ImageSource;

pub struct PPMissingEmbed {
    description: Option<String>,
    title: Option<String>,
    thumbnail: Option<ImageSource>,
    author: Option<Author>,
    footer: Option<Footer>,
}

impl PPMissingEmbed {
    pub fn new(user: User, scores: Vec<Score>, pp: f32, rank: Option<usize>) -> Self {
        let title = format!(
            "What score is {name} missing to reach {pp_given}pp?",
            name = user.username,
            pp_given = with_comma(pp),
        );

        let description = if scores.is_empty() {
            format!(
                "To reach {pp}pp with one additional score, {user} needs to perform \
                 a **{pp}pp** score which would be the top #1",
                pp = with_comma(pp),
                user = user.username,
            )
        } else if user.pp_raw > pp {
            format!(
                "{name} has {pp_raw}pp which is already more than {pp_given}pp.",
                name = user.username,
                pp_raw = with_comma(user.pp_raw),
                pp_given = with_comma(pp)
            )
        } else {
            let (required, idx) = pp_missing(user.pp_raw, pp, &scores);

            format!(
                "To reach {pp}pp with one additional score, {user} needs to perform \
                 a **{required}pp** score which would be the top #{idx}",
                pp = with_comma(pp),
                user = user.username,
                required = with_comma(required),
                idx = idx
            )
        };

        let footer = if let Some(rank) = rank {
            Some(Footer::new(format!(
                "The current rank for {pp}pp is #{rank}",
                pp = with_comma(pp),
                rank = with_comma_u64(rank as u64),
            )))
        } else {
            None
        };

        Self {
            title: Some(title),
            footer,
            description: Some(description),
            author: Some(osu::get_user_author(&user)),
            thumbnail: Some(ImageSource::url(format!("{}{}", AVATAR_URL, user.user_id)).unwrap()),
        }
    }
}

impl EmbedData for PPMissingEmbed {
    fn description_owned(&mut self) -> Option<String> {
        self.description.take()
    }
    fn thumbnail_owned(&mut self) -> Option<ImageSource> {
        self.thumbnail.take()
    }
    fn author_owned(&mut self) -> Option<Author> {
        self.author.take()
    }
    fn title_owned(&mut self) -> Option<String> {
        self.title.take()
    }
    fn footer_owned(&mut self) -> Option<Footer> {
        self.footer.take()
    }
}
