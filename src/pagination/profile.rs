use super::PageChange;

use crate::{
    embeds::{EmbedData, ProfileEmbed},
    unwind_error, BotResult, Context, CONFIG,
};

use std::time::Duration;
use tokio::stream::StreamExt;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::{
    channel::{Message, Reaction, ReactionType},
    gateway::payload::ReactionAdd,
    id::{EmojiId, UserId},
};

pub struct ProfilePagination {
    msg: Message,
    embed: ProfileEmbed,
    minimized: bool,
}

impl ProfilePagination {
    pub fn new(msg: Message, embed: ProfileEmbed) -> Self {
        Self {
            msg,
            embed,
            minimized: true,
        }
    }

    fn reactions() -> Vec<RequestReactionType> {
        let config = CONFIG.get().unwrap();

        let (min_id, min_name) = config.minimize();
        let (id, name) = (EmojiId(min_id), Some(min_name.to_owned()));
        let minimize = RequestReactionType::Custom { id, name };

        let (exp_id, exp_name) = config.expand();
        let (id, name) = (EmojiId(exp_id), Some(exp_name.to_owned()));
        let expand = RequestReactionType::Custom { id, name };

        vec![expand, minimize]
    }

    pub async fn start(mut self, ctx: &Context, owner: UserId, duration: u64) -> BotResult<()> {
        ctx.store_msg(self.msg.id);

        let mut reaction_stream = {
            for emoji in Self::reactions() {
                ctx.http
                    .create_reaction(self.msg.channel_id, self.msg.id, emoji)
                    .await?;
            }
            ctx.standby
                .wait_for_reaction_stream(self.msg.id, move |r: &ReactionAdd| r.0.user_id == owner)
                .timeout(Duration::from_secs(duration))
        };

        while let Some(Ok(reaction)) = reaction_stream.next().await {
            match self.next_page(reaction.0, ctx).await {
                Ok(PageChange::Delete) => return Ok(()),
                Ok(_) => {}
                Err(why) => unwind_error!(warn, why, "Error while paginating profile: {}"),
            }
        }

        if !ctx.remove_msg(self.msg.id) {
            return Ok(());
        }

        for emoji in Self::reactions() {
            if self.msg.guild_id.is_none() {
                ctx.http
                    .delete_current_user_reaction(self.msg.channel_id, self.msg.id, emoji)
                    .await?;
            } else {
                ctx.http
                    .delete_all_reaction(self.msg.channel_id, self.msg.id, emoji)
                    .await?;
            }
        }

        if !self.minimized {
            let eb = self.embed.minimize();

            ctx.http
                .update_message(self.msg.channel_id, self.msg.id)
                .embed(eb.build()?)?
                .await?;
        }

        Ok(())
    }

    async fn next_page(&mut self, reaction: Reaction, ctx: &Context) -> BotResult<PageChange> {
        let change = match self.process_reaction(&reaction.emoji).await {
            PageChange::None => PageChange::None,
            PageChange::Change => {
                let eb = if self.minimized {
                    self.embed.minimize_borrowed()
                } else {
                    self.embed.build()
                };

                ctx.http
                    .update_message(self.msg.channel_id, self.msg.id)
                    .embed(eb.build()?)?
                    .await?;

                PageChange::Change
            }
            PageChange::Delete => {
                ctx.http
                    .delete_message(self.msg.channel_id, self.msg.id)
                    .await?;

                PageChange::Delete
            }
        };

        Ok(change)
    }

    async fn process_reaction(&mut self, reaction: &ReactionType) -> PageChange {
        let change_result = match reaction {
            ReactionType::Custom {
                name: Some(name), ..
            } => match name.as_str() {
                "expand" => match self.minimized {
                    true => Some(false),
                    false => None,
                },
                "minimize" => match self.minimized {
                    true => None,
                    false => Some(true),
                },
                _ => return PageChange::None,
            },
            ReactionType::Unicode { name } if name == "❌" => return PageChange::Delete,
            _ => return PageChange::None,
        };

        match change_result {
            Some(min) => {
                self.minimized = min;

                PageChange::Change
            }
            None => PageChange::None,
        }
    }
}
