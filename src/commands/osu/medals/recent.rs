use crate::{
    bail,
    embeds::{EmbedData, MedalEmbed},
    pagination::{MedalRecentPagination, Pagination},
    util::{
        constants::{GENERAL_ISSUE, OSEKAI_ISSUE, OSU_API_ISSUE},
        MessageExt,
    },
    Args, BotResult, CommandData, Context, MessageBuilder, Name,
};

use chrono::{DateTime, Utc};
use rosu_v2::prelude::{OsuError, User};
use std::{cmp::Reverse, sync::Arc};

#[command]
#[short_desc("Display a recently acquired medal of a user")]
#[long_desc(
    "Display a recently acquired medal of a user.\n\
    To start from a certain index, specify a number right after the command, e.g. `mr3`."
)]
#[usage("[username]")]
#[example("badewanne3", r#""im a fancy lad""#)]
#[aliases("mr", "recentmedal")]
async fn medalrecent(ctx: Arc<Context>, data: CommandData) -> BotResult<()> {
    match data {
        CommandData::Message { msg, mut args, num } => match RecentArgs::args(&ctx, &mut args, num)
        {
            Ok(recent_args) => {
                _medalrecent(ctx, CommandData::Message { msg, args, num }, recent_args).await
            }
            Err(content) => msg.error(&ctx, content).await,
        },
        CommandData::Interaction { command } => super::slash_medal(ctx, command).await,
    }
}

pub(super) async fn _medalrecent(
    ctx: Arc<Context>,
    data: CommandData<'_>,
    args: RecentArgs,
) -> BotResult<()> {
    let RecentArgs { name, index } = args;

    let name = match name {
        Some(name) => name,
        None => match ctx.get_link(data.author()?.id.0) {
            Some(name) => name,
            None => return super::require_link(&ctx, &data).await,
        },
    };

    let user_fut = super::request_user(&ctx, &name, None);
    let medals_fut = ctx.psql().get_medals();

    let (mut user, all_medals) = match tokio::join!(user_fut, medals_fut) {
        (Ok(user), Ok(medals)) => (user, medals),
        (Err(OsuError::NotFound), _) => {
            let content = format!("User `{}` was not found", name);

            return data.error(&ctx, content).await;
        }
        (_, Err(why)) => {
            let _ = data.error(&ctx, GENERAL_ISSUE).await;

            return Err(why);
        }
        (Err(why), _) => {
            let _ = data.error(&ctx, OSU_API_ISSUE).await;

            return Err(why.into());
        }
    };

    let mut achieved_medals = match user.medals.take() {
        Some(medals) => medals,
        None => {
            let _ = data.error(&ctx, GENERAL_ISSUE).await;

            bail!("Empty medals option on user");
        }
    };

    achieved_medals.sort_unstable_by_key(|medal| Reverse(medal.achieved_at));
    let index = index.unwrap_or(1);

    let (medal, achieved_at) = match achieved_medals.get(index - 1) {
        Some(achieved) => {
            let medal = match all_medals.get(&achieved.medal_id) {
                Some(medal) => medal,
                None => {
                    let _ = data.error(&ctx, GENERAL_ISSUE).await;

                    bail!("Missing medal id {} in DB medals", achieved.medal_id);
                }
            };

            match ctx.clients.custom.get_osekai_medal(&medal.name).await {
                Ok(Some(medal)) => (medal, achieved.achieved_at),
                Ok(None) => {
                    let _ = data.error(&ctx, GENERAL_ISSUE).await;

                    bail!("No osekai medal for DB medal `{}`", medal.name);
                }
                Err(why) => {
                    let _ = data.error(&ctx, OSEKAI_ISSUE).await;

                    return Err(why.into());
                }
            }
        }
        None => {
            let content = format!(
                "`{}` only has {} medals, cannot show medal #{}",
                user.username,
                achieved_medals.len(),
                index
            );

            return data.error(&ctx, content).await;
        }
    };

    let content = match index % 10 {
        1 if index == 1 => "Most recent medal:".to_owned(),
        1 if index != 11 => format!("{}st most recent medal:", index),
        2 if index != 12 => format!("{}nd most recent medal:", index),
        3 if index != 13 => format!("{}rd most recent medal:", index),
        _ => format!("{}th most recent medal:", index),
    };

    let achieved = MedalAchieved {
        user: &user,
        achieved_at,
        index,
        medal_count: achieved_medals.len(),
    };

    let embed_data = MedalEmbed::new(medal, Some(achieved), false);
    let embed = embed_data.clone().into_builder().build();
    let builder = MessageBuilder::new().embed(embed.clone()).content(content);
    let response = data.create_message(&ctx, builder).await?.model().await?;
    let owner = data.author()?.id;

    let pagination = MedalRecentPagination::new(
        Arc::clone(&ctx),
        response,
        user,
        all_medals,
        achieved_medals,
        index,
        embed_data,
    );

    tokio::spawn(async move {
        if let Err(why) = pagination.start(&ctx, owner, 60).await {
            unwind_error!(warn, why, "Pagination error (medalrecent): {}")
        }
    });

    Ok(())
}

pub(super) struct RecentArgs {
    pub name: Option<Name>,
    pub index: Option<usize>,
}

impl RecentArgs {
    fn args(ctx: &Context, args: &mut Args, index: Option<usize>) -> Result<Self, &'static str> {
        let name = args
            .next()
            .map(|arg| Args::try_link_name(ctx, arg))
            .transpose()?;

        Ok(Self { name, index })
    }
}

pub struct MedalAchieved<'u> {
    pub user: &'u User,
    pub achieved_at: DateTime<Utc>,
    pub index: usize,
    pub medal_count: usize,
}