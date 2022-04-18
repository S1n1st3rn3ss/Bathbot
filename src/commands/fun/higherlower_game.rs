use command_macros::SlashCommand;
use rosu_v2::prelude::GameMode;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::application::interaction::ApplicationCommand;

use crate::{
    commands::GameModeOption,
    games::hl::{GameState, HlComponents},
    util::{
        builder::{EmbedBuilder, MessageBuilder},
        constants::{GENERAL_ISSUE, RED},
        ApplicationCommandExt, Authored,
    },
    BotResult, Context,
};

use std::{fmt::Display, sync::Arc};

#[derive(CommandModel, CreateCommand, SlashCommand)]
#[command(
    name = "higherlower",
    help = "Play a game of osu! themed higher lower.\n\
    The available versions are:\n \
    - `Score PP`: Guess whether the next play is worth higher or lower PP"
)]
/// Play a game of osu! themed higher lower
pub struct HigherLower {
    /// Specify a gamemode
    mode: Option<GameModeOption>,
}

async fn slash_higherlower(
    ctx: Arc<Context>,
    mut command: Box<ApplicationCommand>,
) -> BotResult<()> {
    let user = command.user_id()?;

    let give_up_content = ctx.hl_games().lock().await.get(&user).map(|game| {
        let GameState { guild, channel, msg: id, .. } = game;

        format!(
            "You can't play two higherlower games at once! \n\
            Finish your [other game](https://discord.com/channels/{}/{channel}/{id}) first or give up.",
            match guild {
                Some(ref id) => id as &dyn Display,
                None => &"@me" as &dyn Display,
            },
        )
    });

    if let Some(content) = give_up_content {
        let components = HlComponents::give_up();
        let embed = EmbedBuilder::new().color(RED).description(content).build();

        let builder = MessageBuilder::new().embed(embed).components(components);
        command.update(&ctx, &builder).await?;
    } else {
        let args = HigherLower::from_interaction(command.input_data())?;

        let mode = match args.mode.map(GameMode::from) {
            Some(mode) => mode,
            None => ctx.user_config(user).await?.mode.unwrap_or(GameMode::STD),
        };

        let mut game = match GameState::score_pp(&ctx, &*command, mode).await {
            Ok(game) => game,
            Err(err) => {
                let _ = command.error(&ctx, GENERAL_ISSUE).await;

                return Err(err);
            }
        };

        let embed = game.to_embed().await;
        let components = HlComponents::higherlower();
        let builder = MessageBuilder::new().embed(embed).components(components);

        let response = command.update(&ctx, &builder).await?.model().await?;

        game.msg = response.id;
        ctx.hl_games().lock().await.insert(user, game);
    }

    Ok(())
}
