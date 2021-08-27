use crate::{
    embeds::{EmbedData, MatchCostEmbed},
    util::{constants::OSU_API_ISSUE, matcher, ApplicationCommandExt, MessageExt},
    Args, BotResult, CommandData, Context, Error, MessageBuilder,
};

use hashbrown::{HashMap, HashSet};
use rosu_v2::prelude::{
    GameMods, MatchGame, Osu, OsuError, OsuMatch, OsuResult, Team, TeamType, UserCompact,
};
use std::{cmp::Ordering, collections::HashMap as StdHashMap, fmt::Write, mem, sync::Arc};
use twilight_model::application::{
    command::{ChoiceCommandOptionData, Command, CommandOption},
    interaction::{application_command::CommandDataOption, ApplicationCommand},
};

const USER_LIMIT: usize = 50;
const TOO_MANY_PLAYERS_TEXT: &str = "Too many players, cannot display message :(";

#[command]
#[short_desc("Display performance ratings for a multiplayer match")]
#[long_desc(
    "Calculate a performance rating for each player \
     in the given multiplayer match. The optional second \
     argument is the amount of played warmups, defaults to 2.\n\
     Here's the current [formula](https://i.imgur.com/7KFwcUS.png).\n\
     Keep in mind that all bots use different formulas so comparing \
     with values from other bots makes no sense."
)]
#[usage("[match url / match id] [amount of warmups]")]
#[example("58320988 1", "https://osu.ppy.sh/community/matches/58320988")]
#[aliases("mc", "matchcost")]
async fn matchcosts(ctx: Arc<Context>, data: CommandData) -> BotResult<()> {
    match data {
        CommandData::Message { msg, mut args, num } => match MatchCostArgs::args(&mut args) {
            Ok(matchcost_args) => {
                _matchcosts(ctx, CommandData::Message { msg, args, num }, matchcost_args).await
            }
            Err(content) => msg.error(&ctx, content).await,
        },
        CommandData::Interaction { command } => slash_matchcost(ctx, *command).await,
    }
}

async fn _matchcosts(
    ctx: Arc<Context>,
    data: CommandData<'_>,
    args: MatchCostArgs,
) -> BotResult<()> {
    let match_id = args.match_id;
    let warmups = args.warmups;

    // Retrieve the match
    let (mut osu_match, games) = match ctx.osu().osu_match(match_id).await {
        Ok(mut osu_match) => {
            retrieve_previous(&mut osu_match, ctx.osu()).await?;
            let games: Vec<_> = osu_match.drain_games().skip(warmups).collect();

            (osu_match, games)
        }
        Err(OsuError::NotFound) => {
            let content = "Either the mp id was invalid or the match was private";

            return data.error(&ctx, content).await;
        }
        Err(why) => {
            let _ = data.error(&ctx, OSU_API_ISSUE).await;

            return Err(why.into());
        }
    };

    // Count different users
    let users: HashSet<_> = games
        .iter()
        .map(|game| game.scores.iter())
        .flatten()
        .filter(|s| s.score > 0)
        .map(|s| s.user_id)
        .collect();

    // Prematurely abort if its too many players to display in a message
    if users.len() > USER_LIMIT {
        return data.error(&ctx, TOO_MANY_PLAYERS_TEXT).await;
    }

    // Process match
    let (description, match_result) = if games.is_empty() {
        let mut description = format!("No games played yet beyond the {} warmup", warmups);

        if warmups != 1 {
            description.push('s');
        }

        (Some(description), None)
    } else {
        let result = process_match(&games, osu_match.end_time.is_some(), &osu_match.users);

        (None, Some(result))
    };

    // Accumulate all necessary data
    let embed_data = match MatchCostEmbed::new(&mut osu_match, description, match_result) {
        Some(data) => data,
        None => return data.error(&ctx, TOO_MANY_PLAYERS_TEXT).await,
    };

    let embed = embed_data.into_builder().build();

    let content = (warmups > 0).then(|| {
        let mut content = "Ignoring the first ".to_owned();

        if warmups == 1 {
            content.push_str("map");
        } else {
            let _ = write!(content, "{} maps", warmups);
        }

        content.push_str(" as warmup:");

        content
    });

    // Creating the embed
    let mut builder = MessageBuilder::new().embed(embed);

    if let Some(content) = content {
        builder = builder.content(content);
    }

    data.create_message(&ctx, builder).await?;

    Ok(())
}

async fn retrieve_previous(osu_match: &mut OsuMatch, osu: &Osu) -> OsuResult<()> {
    let mut curr = &*osu_match;
    let mut prev: Option<OsuMatch> = None;

    // Retrieve at most 500 previous events
    for _ in 0..5 {
        match curr.get_previous(osu).await {
            Some(Ok(next_prev)) => {
                let prev_opt = prev.take();
                curr = &*prev.get_or_insert(next_prev);

                if let Some(mut prev) = prev_opt {
                    prev.events.append(&mut osu_match.events);
                    mem::swap(&mut prev.events, &mut osu_match.events);
                    osu_match.users.extend(prev.users);
                }
            }
            Some(Err(why)) => return Err(why),
            None => break,
        }
    }

    if let Some(mut prev) = prev {
        prev.events.append(&mut osu_match.events);
        mem::swap(&mut prev.events, &mut osu_match.events);
        osu_match.users.extend(prev.users);
    }

    Ok(())
}

macro_rules! sort {
    ($slice:expr) => {
        $slice.sort_unstable_by(|(.., a), (.., b)| b.partial_cmp(a).unwrap_or(Ordering::Equal));
    };
}

// flat additive bonus for each participated game
const FLAT_PARTICIPATION_BONUS: f32 = 0.5;

// exponent base, the higher - the higher is the difference
// between players who played a lot and players who played fewer
const BASE_PARTICIPATION_BONUS: f32 = 1.4;

// exponent, low: logithmically ~ high: linear
const EXP_PARTICIPATION_BONUS: f32 = 0.6;

// instead of considering tb score once, consider it this many times
const TIEBREAKER_BONUS: f32 = 2.0;

// global multiplier per combination (if at least 3)
const MOD_BONUS: f32 = 0.02;

pub fn process_match(
    games: &[MatchGame],
    finished: bool,
    users: &StdHashMap<u32, UserCompact>,
) -> MatchResult {
    let mut teams = HashMap::new();
    let mut point_costs = HashMap::new();
    let mut mods = HashMap::new();
    let team_vs = games[0].team_type == TeamType::TeamVS;
    let mut match_scores = MatchScores(0, 0);

    // Calculate point scores for each score in each game
    for game in games.iter() {
        let score_sum: u32 = game.scores.iter().map(|s| s.score).sum();
        let avg = score_sum as f32 / game.scores.iter().filter(|s| s.score > 0).count() as f32;
        let mut team_scores = HashMap::with_capacity(team_vs as usize + 1);

        for score in game.scores.iter().filter(|s| s.score > 0) {
            mods.entry(score.user_id)
                .or_insert_with(HashSet::new)
                .insert(score.mods - GameMods::NoFail);

            let point_cost = score.score as f32 / avg + FLAT_PARTICIPATION_BONUS;

            point_costs
                .entry(score.user_id)
                .or_insert_with(Vec::new)
                .push(point_cost);

            teams.entry(score.user_id).or_insert(score.team);

            team_scores
                .entry(score.team)
                .and_modify(|e| *e += score.score)
                .or_insert(score.score);
        }

        let (winner_team, _) = team_scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .unwrap_or((Team::None, 0));

        match_scores.incr(winner_team);
    }

    // Tiebreaker bonus
    if finished && games.len() > 2 && match_scores.difference() == 1 {
        let game = games.last().unwrap();

        point_costs
            .iter_mut()
            .filter(|(&user_id, _)| game.scores.iter().any(|score| score.user_id == user_id))
            .map(|(_, costs)| costs.last_mut().unwrap())
            .for_each(|value| *value = (*value * TIEBREAKER_BONUS) - FLAT_PARTICIPATION_BONUS);
    }

    // Mod combinations bonus
    let mods_count = mods
        .into_iter()
        .filter(|(_, mods)| mods.len() > 2)
        .map(|(id, mods)| (id, mods.len() - 2));

    for (user_id, count) in mods_count {
        let multiplier = 1.0 + count as f32 * MOD_BONUS;

        point_costs.entry(user_id).and_modify(|point_scores| {
            point_scores
                .iter_mut()
                .for_each(|point_score| *point_score *= multiplier);
        });
    }

    // Calculate match costs by combining point costs
    let mut data = HashMap::with_capacity(team_vs as usize + 1);
    let mut highest_cost = 0.0;
    let mut mvp_avatar_url = None;

    for (user_id, point_costs) in point_costs {
        let sum: f32 = point_costs.iter().sum();
        let costs_len = point_costs.len() as f32;
        let mut match_cost = sum / costs_len;

        let exp = match games.len() {
            1 => 0.0,
            len => (costs_len - 1.0) / (len as f32 - 1.0),
        };

        match_cost *= BASE_PARTICIPATION_BONUS.powf(exp.powf(EXP_PARTICIPATION_BONUS));

        data.entry(*teams.get(&user_id).unwrap())
            .or_insert_with(Vec::new)
            .push((user_id, match_cost));

        if match_cost > highest_cost {
            highest_cost = match_cost;

            if let Some(user) = users.get(&user_id) {
                mvp_avatar_url.replace(user.avatar_url.as_str());
            }
        }
    }

    let mvp_avatar_url = mvp_avatar_url.map_or_else(String::new, |url| url.to_owned());

    if team_vs {
        let blue = match data.remove(&Team::Blue) {
            Some(mut team) => {
                sort!(team);

                team
            }
            None => Vec::new(),
        };

        let red = match data.remove(&Team::Red) {
            Some(mut team) => {
                sort!(team);

                team
            }
            None => Vec::new(),
        };

        MatchResult::team(mvp_avatar_url, match_scores, blue, red)
    } else {
        let mut players = data.remove(&Team::None).unwrap_or_default();
        sort!(players);

        MatchResult::solo(mvp_avatar_url, players)
    }
}

type PlayerResult = (u32, f32);
type TeamResult = Vec<PlayerResult>;

pub enum MatchResult {
    TeamVS {
        blue: TeamResult,
        red: TeamResult,
        mvp_avatar_url: String,
        match_scores: MatchScores,
    },
    HeadToHead {
        players: TeamResult,
        mvp_avatar_url: String,
    },
}

impl MatchResult {
    fn team(
        mvp_avatar_url: String,
        match_scores: MatchScores,
        blue: TeamResult,
        red: TeamResult,
    ) -> Self {
        Self::TeamVS {
            mvp_avatar_url,
            match_scores,
            blue,
            red,
        }
    }

    fn solo(mvp_avatar_url: String, players: TeamResult) -> Self {
        Self::HeadToHead {
            mvp_avatar_url,
            players,
        }
    }
}

#[derive(Copy, Clone)]
pub struct MatchScores(u8, u8);

impl MatchScores {
    fn incr(&mut self, team: Team) {
        match team {
            Team::Blue => self.0 = self.0.saturating_add(1),
            Team::Red => self.1 = self.1.saturating_add(1),
            Team::None => {}
        }
    }

    pub fn blue(self) -> u8 {
        self.0
    }

    pub fn red(self) -> u8 {
        self.1
    }

    fn difference(&self) -> u8 {
        let min = self.0.min(self.1);
        let max = self.0.max(self.1);

        max - min
    }
}

struct MatchCostArgs {
    match_id: u32,
    warmups: usize,
}

impl MatchCostArgs {
    fn args(args: &mut Args) -> Result<Self, &'static str> {
        let match_id = match args.next().and_then(|arg| matcher::get_osu_match_id(arg)) {
            Some(id) => id,
            None => {
                return Err("The first argument must be either a match \
                    id or the multiplayer link to a match")
            }
        };

        let warmups = args.next().and_then(|num| num.parse().ok()).unwrap_or(2);

        Ok(Self { match_id, warmups })
    }

    fn slash(command: &mut ApplicationCommand) -> BotResult<Result<Self, &'static str>> {
        let mut match_id = None;
        let mut warmups = None;

        for option in command.yoink_options() {
            match option {
                CommandDataOption::String { name, value } => match name.as_str() {
                    "match_url" => match matcher::get_osu_match_id(value.as_str()) {
                        Some(id) => match_id = Some(id),
                        None => {
                            let content = "Failed to parse `match_url`.\n\
                                Be sure it's a valid mp url or a match id.";

                            return Ok(Err(content));
                        }
                    },
                    _ => bail_cmd_option!("matchcost", string, name),
                },
                CommandDataOption::Integer { name, value } => match name.as_str() {
                    "warmups" => warmups = Some(value.max(0) as usize),
                    _ => bail_cmd_option!("matchcost", integer, name),
                },
                CommandDataOption::Boolean { name, .. } => {
                    bail_cmd_option!("matchcost", boolean, name)
                }
                CommandDataOption::SubCommand { name, .. } => {
                    bail_cmd_option!("matchcost", subcommand, name)
                }
            }
        }

        let args = MatchCostArgs {
            match_id: match_id.ok_or(Error::InvalidCommandOptions)?,
            warmups: warmups.unwrap_or(2),
        };

        Ok(Ok(args))
    }
}

pub async fn slash_matchcost(ctx: Arc<Context>, mut command: ApplicationCommand) -> BotResult<()> {
    match MatchCostArgs::slash(&mut command)? {
        Ok(args) => _matchcosts(ctx, command.into(), args).await,
        Err(content) => command.error(&ctx, content).await,
    }
}

pub fn slash_matchcost_command() -> Command {
    Command {
        application_id: None,
        guild_id: None,
        name: "matchcost".to_owned(),
        default_permission: None,
        description: "Display performance ratings for a multiplayer match".to_owned(),
        id: None,
        options: vec![
            CommandOption::String(ChoiceCommandOptionData {
                choices: vec![],
                description: "Specify a match url or match id".to_owned(),
                name: "match_url".to_owned(),
                required: true,
            }),
            CommandOption::Integer(ChoiceCommandOptionData {
                choices: vec![],
                description: "Specify the amount of warmups to ignore".to_owned(),
                name: "warmups".to_owned(),
                required: false,
            }),
        ],
    }
}
