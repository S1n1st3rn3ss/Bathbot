use crate::{
    embeds::{EmbedData, TrackingStatsEmbed},
    util::MessageExt,
    Args, BotResult, Context,
};

use std::sync::Arc;
use twilight_model::channel::Message;

#[command]
#[short_desc("Display stats about osu!tracking")]
#[owner()]
async fn trackingstats(ctx: Arc<Context>, msg: &Message, _: Args) -> BotResult<()> {
    let stats = ctx.tracking().stats().await;
    let embed = TrackingStatsEmbed::new(stats).build_owned().build()?;
    msg.build_response(&ctx, |m| m.embed(embed)).await?;
    Ok(())
}
