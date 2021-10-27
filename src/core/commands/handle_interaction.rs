use super::ProcessResult;
use crate::{
    commands::{fun, help, osu, owner, songs, tracking, twitch, utility},
    core::buckets::BucketName,
    embeds::EmbedBuilder,
    util::{
        constants::{
            common_literals::{HELP, MAP, PROFILE},
            OWNER_USER_ID, RED,
        },
        Authored, InteractionExt,
    },
    BotResult, Context, Error,
};

use std::{future::Future, mem, sync::Arc};
use twilight_model::{
    application::{
        callback::{CallbackData, InteractionResponse},
        interaction::{ApplicationCommand, MessageComponentInteraction},
    },
    channel::message::MessageFlags,
    guild::Permissions,
};

struct CommandArgs {
    authority: bool,
    bucket: Option<BucketName>,
    // defer_msg: bool,
    ephemeral: bool,
    only_guilds: bool,
    only_owner: bool,
}

impl Default for CommandArgs {
    fn default() -> Self {
        Self {
            authority: false,
            bucket: None,
            // defer_msg: true,
            ephemeral: false,
            only_guilds: false,
            only_owner: false,
        }
    }
}

pub async fn handle_component(
    ctx: Arc<Context>,
    component: Box<MessageComponentInteraction>,
) -> BotResult<()> {
    let name = component.data.custom_id.as_str();
    log_interaction(&ctx, &*component, name);
    ctx.stats.increment_component(name);

    match name {
        "help_menu" | "help_back" => help::handle_menu_select(&ctx, *component).await,
        _ => Err(Error::UnknownMessageComponent { component }),
    }
}

pub async fn handle_command(ctx: Arc<Context>, mut command: ApplicationCommand) -> BotResult<()> {
    let name = mem::take(&mut command.data.name);
    log_interaction(&ctx, &command, &name);
    ctx.stats.increment_slash_command(&name);

    let mut args = CommandArgs::default();

    let command_result = match name.as_str() {
        "authorities" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, utility::slash_authorities).await
        }
        "avatar" => process_command(ctx, command, args, osu::slash_avatar).await,
        "bws" => process_command(ctx, command, args, osu::slash_bws).await,
        "commands" => process_command(ctx, command, args, utility::slash_commands).await,
        "compare" => process_command(ctx, command, args, osu::slash_compare).await,
        "config" => {
            args.ephemeral = true;

            process_command(ctx, command, args, utility::slash_config).await
        }
        "fix" => process_command(ctx, command, args, osu::slash_fix).await,
        HELP => {
            // Necessary to be able to use data.create_message later on
            start_thinking_ephemeral(&ctx, &command).await?;

            help::slash_help(ctx, command)
                .await
                .map(|_| ProcessResult::Success)
        }
        "invite" => process_command(ctx, command, args, utility::slash_invite).await,
        "leaderboard" => process_command(ctx, command, args, osu::slash_leaderboard).await,
        "link" => {
            args.ephemeral = true;

            process_command(ctx, command, args, osu::slash_link).await
        }
        MAP => process_command(ctx, command, args, osu::slash_map).await,
        "matchcost" => process_command(ctx, command, args, osu::slash_matchcost).await,
        "matchlive" => {
            args.authority = true;

            process_command(ctx, command, args, osu::slash_matchlive).await
        }
        "medal" => process_command(ctx, command, args, osu::slash_medal).await,
        "minesweeper" => process_command(ctx, command, args, fun::slash_minesweeper).await,
        "mostplayed" => process_command(ctx, command, args, osu::slash_mostplayed).await,
        "osekai" => process_command(ctx, command, args, osu::slash_osekai).await,
        "osustats" => process_command(ctx, command, args, osu::slash_osustats).await,
        "owner" => {
            args.only_owner = true;
            args.ephemeral = true;

            process_command(ctx, command, args, owner::slash_owner).await
        }
        "ping" => process_command(ctx, command, args, utility::slash_ping).await,
        PROFILE => process_command(ctx, command, args, osu::slash_profile).await,
        "prune" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, utility::slash_prune).await
        }
        "ranking" => process_command(ctx, command, args, osu::slash_ranking).await,
        "ratios" => process_command(ctx, command, args, osu::slash_ratio).await,
        "reach" => process_command(ctx, command, args, osu::slash_reach).await,
        "recent" => process_command(ctx, command, args, osu::slash_recent).await,
        "roleassign" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, utility::slash_roleassign).await
        }
        "roll" => process_command(ctx, command, args, utility::slash_roll).await,
        "search" => process_command(ctx, command, args, osu::slash_mapsearch).await,
        "simulate" => process_command(ctx, command, args, osu::slash_simulate).await,
        "snipe" => {
            args.bucket = Some(BucketName::Snipe);

            process_command(ctx, command, args, osu::slash_snipe).await
        }
        "song" => {
            args.bucket = Some(BucketName::Songs);

            process_command(ctx, command, args, songs::slash_song).await
        }
        "togglesongs" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, utility::slash_togglesongs).await
        }
        "top" => process_command(ctx, command, args, osu::slash_top).await,
        "track" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, tracking::slash_track).await
        }
        "trackstream" => {
            args.authority = true;
            args.only_guilds = true;

            process_command(ctx, command, args, twitch::slash_trackstream).await
        }
        "whatif" => process_command(ctx, command, args, osu::slash_whatif).await,
        _ => {
            return Err(Error::UnknownSlashCommand {
                name,
                command: Box::new(command),
            });
        }
    };

    match command_result {
        Ok(ProcessResult::Success) => info!("Processed slash command `{}`", name),
        Ok(result) => info!("Command `/{}` was not processed: {:?}", name, result),
        Err(why) => return Err(Error::Command(Box::new(why), name)),
    }

    Ok(())
}

async fn process_command<F, R>(
    ctx: Arc<Context>,
    command: ApplicationCommand,
    args: CommandArgs,
    fun: F,
) -> BotResult<ProcessResult>
where
    F: Fn(Arc<Context>, ApplicationCommand) -> R,
    R: Future<Output = BotResult<()>>,
{
    let ephemeral = args.ephemeral;

    match pre_process_command(&ctx, &command, args).await? {
        Some(result) => Ok(result),
        None => {
            // Let discord know the command is now being processed
            if ephemeral {
                start_thinking_ephemeral(&ctx, &command).await?;
            } else {
                start_thinking(&ctx, &command).await?;
            }

            // Call command function
            (fun)(ctx, command).await?;

            Ok(ProcessResult::Success)
        }
    }
}

async fn start_thinking(ctx: &Context, command: &ApplicationCommand) -> BotResult<()> {
    let response = InteractionResponse::DeferredChannelMessageWithSource(CallbackData {
        allowed_mentions: None,
        components: None,
        content: None,
        embeds: Vec::new(),
        flags: None,
        tts: None,
    });

    ctx.http
        .interaction_callback(command.id, &command.token, &response)
        .exec()
        .await?;

    Ok(())
}

async fn start_thinking_ephemeral(ctx: &Context, command: &ApplicationCommand) -> BotResult<()> {
    let response = InteractionResponse::DeferredChannelMessageWithSource(CallbackData {
        allowed_mentions: None,
        components: None,
        content: None,
        embeds: Vec::new(),
        flags: Some(MessageFlags::EPHEMERAL),
        tts: None,
    });

    ctx.http
        .interaction_callback(command.id, &command.token, &response)
        .exec()
        .await?;

    Ok(())
}

async fn premature_error(
    ctx: &Context,
    command: &ApplicationCommand,
    content: impl Into<String>,
) -> BotResult<()> {
    let embed = EmbedBuilder::new().color(RED).description(content).build();

    let response = InteractionResponse::ChannelMessageWithSource(CallbackData {
        allowed_mentions: None,
        components: None,
        content: None,
        embeds: vec![embed],
        flags: None,
        tts: None,
    });

    ctx.http
        .interaction_callback(command.id, &command.token, &response)
        .exec()
        .await?;

    Ok(())
}

#[inline(never)]
async fn pre_process_command(
    ctx: &Context,
    command: &ApplicationCommand,
    args: CommandArgs,
) -> BotResult<Option<ProcessResult>> {
    let guild_id = command.guild_id;

    // Only in guilds?
    if args.only_guilds && guild_id.is_none() {
        let content = "That command is only available in guilds";
        premature_error(ctx, command, content).await?;

        return Ok(Some(ProcessResult::NoDM));
    }

    let author_id = command.author().ok_or(Error::MissingInteractionAuthor)?.id;

    // Only for owner?
    if args.only_owner && author_id.0 != OWNER_USER_ID {
        let content = "That command can only be used by the bot owner";
        premature_error(ctx, command, content).await?;

        return Ok(Some(ProcessResult::NoOwner));
    }

    let channel_id = command.channel_id;

    // Does bot have sufficient permissions to send response?
    match ctx.cache.current_user() {
        Some(bot_user) => {
            let permissions =
                ctx.cache
                    .get_channel_permissions_for(bot_user.id, channel_id, guild_id);

            if !permissions.contains(Permissions::SEND_MESSAGES) {
                return Ok(Some(ProcessResult::NoSendPermission));
            }
        }
        None => error!("No CurrentUser in cache for permission check"),
    };

    // Ratelimited?
    {
        let guard = ctx.buckets.get(&BucketName::All).unwrap();
        let mutex = guard.value();
        let mut bucket = mutex.lock();
        let ratelimit = bucket.take(author_id.0);

        if ratelimit > 0 {
            debug!("Ratelimiting user {} for {} seconds", author_id, ratelimit,);

            return Ok(Some(ProcessResult::Ratelimited(BucketName::All)));
        }
    }

    if let Some(bucket) = args.bucket {
        if let Some((cooldown, bucket)) =
            super::_check_ratelimit(ctx, author_id, guild_id, bucket).await
        {
            if !matches!(bucket, BucketName::BgHint) {
                let content = format!("Command on cooldown, try again in {} seconds", cooldown);
                premature_error(ctx, command, content).await?;
            }

            return Ok(Some(ProcessResult::Ratelimited(bucket)));
        }
    }

    // Only for authorities?
    if args.authority {
        match super::_check_authority(ctx, author_id, guild_id).await {
            Ok(None) => {}
            Ok(Some(content)) => {
                premature_error(ctx, command, content).await?;

                return Ok(Some(ProcessResult::NoAuthority));
            }
            Err(why) => {
                let content = "Error while checking authority status";
                let _ = premature_error(ctx, command, content).await;

                return Err(Error::Authority(Box::new(why)));
            }
        }
    }

    Ok(None)
}

fn log_interaction(ctx: &Context, interaction: &dyn InteractionExt, name: &str) {
    let username = interaction.username().unwrap_or("<unknown user>");
    let mut location = String::with_capacity(31);

    match interaction.guild_id().and_then(|id| ctx.cache.guild(id)) {
        Some(guild) => {
            location.push_str(guild.name.as_str());
            location.push(':');

            match ctx.cache.guild_channel(interaction.channel_id()) {
                Some(channel) => location.push_str(channel.name()),
                None => location.push_str("<uncached channel>"),
            }
        }
        None => location.push_str("Private"),
    }

    info!("[{}] {} used `{}` interaction", location, username, name);
}