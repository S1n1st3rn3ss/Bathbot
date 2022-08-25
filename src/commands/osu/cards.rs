use std::{
    fmt::{Debug, Display, Formatter, Result as FmtResult},
    mem,
    sync::Arc,
};

use command_macros::{HasName, SlashCommand};
use handlebars::Handlebars;
use once_cell::sync::Lazy;
use rosu_pp::{
    catch::{CatchPerformanceAttributes, CatchScoreState},
    osu::OsuScoreState,
    taiko::TaikoScoreState,
    Beatmap, CatchPP, ManiaPP, OsuPP, TaikoPP,
};
use rosu_v2::prelude::{GameMode, GameMods, OsuError, Score, User};
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use time::OffsetDateTime;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::id::{marker::UserMarker, Id};

use crate::{
    commands::GameModeOption,
    core::{commands::CommandOrigin, BotConfig, Context},
    embeds::{CardEmbed, EmbedData},
    error::PpError,
    util::{
        builder::MessageBuilder,
        constants::{GENERAL_ISSUE, OSU_API_ISSUE},
        datetime::DATE_FORMAT,
        interaction::InteractionCommand,
        osu::{flag_url_svg, prepare_beatmap_file},
        HtmlToPng, InteractionCommandExt,
    },
    BotResult,
};

use super::{get_user_and_scores, ScoreArgs, UserArgs};

static HTML_TEMPLATE: Lazy<Handlebars<'static>> = Lazy::new(|| {
    let mut handlebars = Handlebars::new();
    let mut path = BotConfig::get().paths.cards.clone();
    path.push("template/template.tmpl");

    handlebars
        .register_template_file("card", path)
        .expect("failed to register card template to handlebars");

    handlebars
});

#[derive(CommandModel, CreateCommand, SlashCommand, HasName)]
#[command(name = "card")]
/// Create a user card
pub struct Card {
    /// Specify a gamemode
    mode: Option<GameModeOption>,
    /// Specify a username
    name: Option<String>,
    #[command(
        help = "Instead of specifying an osu! username with the `name` option, \
        you can use this option to choose a discord user.\n\
        Only works on users who have used the `/link` command."
    )]
    /// Specify a linked discord user
    discord: Option<Id<UserMarker>>,
}

async fn slash_card(ctx: Arc<Context>, mut command: InteractionCommand) -> BotResult<()> {
    let args = Card::from_interaction(command.input_data())?;

    let orig = CommandOrigin::Interaction {
        command: &mut command,
    };

    let (name, mode) = name_mode!(ctx, orig, args);

    let user_args = UserArgs::new(&name, mode);
    let scores_args = ScoreArgs::top(100);

    let (mut user, scores) = match get_user_and_scores(&ctx, user_args, &scores_args).await {
        Ok((user, scores)) => (user, scores),
        Err(OsuError::NotFound) => {
            let content = format!("User `{name}` was not found");

            return orig.error(&ctx, content).await;
        }
        Err(err) => {
            let _ = orig.error(&ctx, OSU_API_ISSUE).await;

            return Err(err.into());
        }
    };

    user.mode = mode;

    let render_data = match Skills::calculate(&ctx, mode, &scores).await {
        Ok(skills) => skills.render_data(&user, &scores),
        Err(err) => {
            let _ = orig.error(&ctx, GENERAL_ISSUE).await;

            return Err(err.into());
        }
    };

    let html = match HTML_TEMPLATE.render("card", &render_data) {
        Ok(rendered) => rendered,
        Err(err) => {
            let _ = orig.error(&ctx, GENERAL_ISSUE).await;

            return Err(err.into());
        }
    };

    let bytes = match HtmlToPng::convert(&html) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = orig.error(&ctx, GENERAL_ISSUE).await;

            return Err(err.into());
        }
    };

    let embed = CardEmbed::new(&user).build();

    let builder = MessageBuilder::new()
        .attachment("card.png", bytes)
        .embed(embed);

    orig.create_message(&ctx, &builder).await?;

    Ok(())
}

#[derive(Copy, Clone)]
enum Skills {
    Osu { acc: f64, aim: f64, speed: f64 },
    Taiko { acc: f64, strain: f64 },
    Catch { acc: f64, movement: f64 },
    Mania { acc: f64, strain: f64 },
}

impl Skills {
    async fn calculate(ctx: &Context, mode: GameMode, scores: &[Score]) -> Result<Self, PpError> {
        // https://www.desmos.com/calculator/gqnhbpa0d3
        let map = |val: f64| {
            let factor = (8.0 / (val / 72.0 + 8.0)).powi(10);

            -101.0 * factor + 101.0
        };

        match mode {
            GameMode::Osu => {
                let mut acc = 0.0;
                let mut aim = 0.0;
                let mut speed = 0.0;
                let mut weight_sum = 0.0;

                const ACC_NERF: f64 = 1.3;
                const AIM_NERF: f64 = 2.6;
                const SPEED_NERF: f64 = 2.4;

                for (i, score) in scores.iter().enumerate() {
                    let map = score.map.as_ref().unwrap();
                    let map_path = prepare_beatmap_file(ctx, map.map_id).await?;
                    let map = Beatmap::from_path(map_path).await?;

                    let state = OsuScoreState {
                        max_combo: score.max_combo as usize,
                        n300: score.statistics.count_300 as usize,
                        n100: score.statistics.count_100 as usize,
                        n50: score.statistics.count_50 as usize,
                        misses: score.statistics.count_miss as usize,
                    };

                    let attrs = OsuPP::new(&map)
                        .mods(score.mods.bits())
                        .state(state)
                        .calculate();

                    let acc_val = attrs.pp_acc / ACC_NERF;
                    let aim_val = attrs.pp_aim / AIM_NERF;
                    let speed_val = attrs.pp_speed / SPEED_NERF;
                    let weight = 0.95_f64.powi(i as i32);

                    acc += acc_val * weight;
                    aim += aim_val * weight;
                    speed += speed_val * weight;
                    weight_sum += weight;
                }

                acc = map(acc / weight_sum);
                aim = map(aim / weight_sum);
                speed = map(speed / weight_sum);

                Ok(Self::Osu { acc, aim, speed })
            }
            GameMode::Taiko => {
                let mut acc = 0.0;
                let mut strain = 0.0;
                let mut weight_sum = 0.0;

                const ACC_NERF: f64 = 1.15;
                const STRAIN_NERF: f64 = 1.6;

                for (i, score) in scores.iter().enumerate() {
                    let map = score.map.as_ref().unwrap();
                    let map_path = prepare_beatmap_file(ctx, map.map_id).await?;
                    let map = Beatmap::from_path(map_path).await?;

                    let state = TaikoScoreState {
                        max_combo: score.max_combo as usize,
                        n300: score.statistics.count_300 as usize,
                        n100: score.statistics.count_100 as usize,
                        misses: score.statistics.count_miss as usize,
                    };

                    let attrs = TaikoPP::new(&map)
                        .mods(score.mods.bits())
                        .state(state)
                        .calculate();

                    let acc_val = attrs.pp_acc / ACC_NERF;
                    let strain_val = attrs.pp_strain / STRAIN_NERF;
                    let weight = 0.95_f64.powi(i as i32);

                    acc += acc_val * weight;
                    strain += strain_val * weight;
                    weight_sum += weight;
                }

                acc = map(acc / weight_sum);
                strain = map(strain / weight_sum);

                Ok(Self::Taiko { acc, strain })
            }
            GameMode::Catch => {
                let mut acc = 0.0;
                let mut movement = 0.0;
                let mut weight_sum = 0.0;

                const ACC_BUFF: f64 = 2.0;
                const MOVEMENT_NERF: f64 = 4.7;

                for (i, score) in scores.iter().enumerate() {
                    let map = score.map.as_ref().unwrap();
                    let map_path = prepare_beatmap_file(ctx, map.map_id).await?;
                    let map = Beatmap::from_path(map_path).await?;

                    let state = CatchScoreState {
                        max_combo: score.max_combo as usize,
                        n_fruits: score.statistics.count_300 as usize,
                        n_droplets: score.statistics.count_100 as usize,
                        n_tiny_droplets: score.statistics.count_50 as usize,
                        n_tiny_droplet_misses: score.statistics.count_katu as usize,
                        misses: score.statistics.count_miss as usize,
                    };

                    let CatchPerformanceAttributes { difficulty, pp } = CatchPP::new(&map)
                        .mods(score.mods.bits())
                        .state(state)
                        .calculate();

                    let acc_ = score.accuracy as f64;
                    let od = map.od as f64;

                    let n_objects = (difficulty.n_fruits
                        + difficulty.n_droplets
                        + difficulty.n_tiny_droplets) as f64;

                    // https://www.desmos.com/calculator/cg59pywpry
                    let acc_exp = ((acc_ / 46.5).powi(6) / 55.0).powf(1.5);
                    let acc_adj = (5.0 * acc_exp.powf(0.1).ln_1p()).recip();

                    let acc_val = difficulty.stars.powf(acc_exp - acc_adj)
                        * (od / 7.0).powf(0.25)
                        * (n_objects / 2000.0).powf(0.15)
                        * ACC_BUFF;

                    let movement_val = pp / MOVEMENT_NERF;
                    let weight = 0.95_f64.powi(i as i32);

                    acc += acc_val * weight;
                    movement += movement_val * weight;
                    weight_sum += weight;
                }

                acc = map(acc / weight_sum);
                movement = map(movement / weight_sum);

                Ok(Self::Catch { acc, movement })
            }
            GameMode::Mania => {
                let mut acc = 0.0;
                let mut strain = 0.0;
                let mut weight_sum = 0.0;

                const ACC_BUFF: f64 = 2.1;
                const STRAIN_NERF: f64 = 6.4;

                for (i, score) in scores.iter().enumerate() {
                    let map = score.map.as_ref().unwrap();
                    let map_path = prepare_beatmap_file(ctx, map.map_id).await?;
                    let map = Beatmap::from_path(map_path).await?;

                    let attrs = ManiaPP::new(&map)
                        .mods(score.mods.bits())
                        .score(score.score)
                        .calculate();

                    let acc_ = score.accuracy as f64;
                    let od = score.map.as_ref().unwrap().od as f64;
                    let n_objects = score.total_hits() as f64;

                    // https://www.desmos.com/calculator/b30p1awwft
                    let acc_ = ((acc_ / 36.0).powf(4.5) / 60.0).powf(1.5);

                    let acc_val = attrs.stars().powf(acc_)
                        * (od / 7.0).powf(0.25)
                        * (n_objects / 2000.0).powf(0.15)
                        * ACC_BUFF;

                    let strain_val = attrs.pp_strain / STRAIN_NERF;
                    let weight = 0.95_f64.powi(i as i32);

                    acc += acc_val * weight;
                    strain += strain_val * weight;
                    weight_sum += weight;
                }

                acc = map(acc / weight_sum);
                strain = map(strain / weight_sum);

                Ok(Self::Mania { acc, strain })
            }
        }
    }

    fn evaluate_title(self, mode: GameMode, scores: &[Score]) -> Title {
        let (max, main) = match self {
            Self::Osu { acc, aim, speed } => {
                let max = acc.max(aim).max(speed);

                (max, TitleMain::osu(acc, aim, speed, max))
            }
            Self::Taiko { acc, strain } => {
                let max = acc.max(strain);

                (max, TitleMain::taiko(acc, strain, max))
            }
            Self::Catch { acc, movement } => {
                let max = acc.max(movement);

                (max, TitleMain::catch(acc, movement, max))
            }
            Self::Mania { acc, strain } => {
                let max = acc.max(strain);

                (max, TitleMain::mania(acc, strain, max))
            }
        };

        let prefix = TitlePrefix::new(max);
        let mods = ModDescriptions::new(mode, scores);

        Title { prefix, mods, main }
    }

    fn render_data(&self, user: &User, scores: &[Score]) -> Value {
        let stats = user.statistics.as_ref().expect("missing user statistics");
        let title = self.evaluate_title(user.mode, scores);
        let path = &BotConfig::get().paths.cards;
        let flag = flag_url_svg(&user.country_code);
        let now = OffsetDateTime::now_utc().format(&DATE_FORMAT).unwrap();
        let background = format!("img/backgrounds/{}.png", title.prefix.background());

        match self {
            Skills::Osu { acc, aim, speed } => {
                json!({
                    "path": path,
                    "gamemode": "mode_standard",
                    "title": title,
                    "username": user.username,
                    "flag": flag,
                    "gamemode_icon": "img/gamemodes/Standard.svg",
                    "user_pfp": user.avatar_url,
                    "accuracy_enabled": "show",
                    "accuracy": acc.trunc(),
                    "accuracy_decimal": (acc.fract() * 100.0).round() as u32,
                    "aim_enabled": "show",
                    "aim": aim.trunc(),
                    "aim_decimal": (aim.fract() * 100.0).round() as u32,
                    "speed_enabled": "show",
                    "speed": speed.trunc(),
                    "speed_decimal": (speed.fract() * 100.0).round() as u32,
                    "strain_enabled": "hidden",
                    "strain": 0.0,
                    "strain_decimal": 0.0,
                    "movement_enabled": "hidden",
                    "movement": 0.0,
                    "movement_decimal": 0.0,
                    "global_rank": stats.global_rank.unwrap_or(0),
                    "country_rank": stats.country_rank.unwrap_or(0),
                    "level": stats.level.current,
                    "level_percentage": stats.level.progress,
                    "date": now,
                    "background_image": background,
                })
            }
            Skills::Taiko { acc, strain } => {
                json!({
                    "path": path,
                    "gamemode": "mode_taiko",
                    "title": title,
                    "username": user.username,
                    "flag": flag,
                    "gamemode_icon": "img/gamemodes/Taiko.svg",
                    "user_pfp": user.avatar_url,
                    "accuracy_enabled": "show",
                    "accuracy": acc.trunc(),
                    "accuracy_decimal": (acc.fract() * 100.0).round() as u32,
                    "aim_enabled": "hidden",
                    "aim": 0.0,
                    "aim_decimal": 0.0,
                    "speed_enabled": "hidden",
                    "speed": 0.0,
                    "speed_decimal": 0.0,
                    "strain_enabled": "show",
                    "strain": strain.trunc(),
                    "strain_decimal": (strain.fract() * 100.0).round() as u32,
                    "movement_enabled": "hidden",
                    "movement": 0.0,
                    "movement_decimal": 0.0,
                    "global_rank": stats.global_rank.unwrap_or(0),
                    "country_rank": stats.country_rank.unwrap_or(0),
                    "level": stats.level.current,
                    "level_percentage": stats.level.progress,
                    "date": now,
                    "background_image": background,
                })
            }
            Skills::Catch { acc, movement } => {
                json!({
                    "path": path,
                    "gamemode": "mode_catch",
                    "title": title,
                    "username": user.username,
                    "flag": flag,
                    "gamemode_icon": "img/gamemodes/Catch.svg",
                    "user_pfp": user.avatar_url,
                    "accuracy_enabled": "show",
                    "accuracy": acc.trunc(),
                    "accuracy_decimal": (acc.fract() * 100.0).round() as u32,
                    "aim_enabled": "hidden",
                    "aim": 0.0,
                    "aim_decimal": 0.0,
                    "speed_enabled": "hidden",
                    "speed": 0.0,
                    "speed_decimal": 0.0,
                    "strain_enabled": "hidden",
                    "strain": 0.0,
                    "strain_decimal": 0.0,
                    "movement_enabled": "show",
                    "movement": movement.trunc(),
                    "movement_decimal": (movement.fract() * 100.0).round() as u32,
                    "global_rank": stats.global_rank.unwrap_or(0),
                    "country_rank": stats.country_rank.unwrap_or(0),
                    "level": stats.level.current,
                    "level_percentage": stats.level.progress,
                    "date": now,
                    "background_image": background,
                })
            }
            Skills::Mania { acc, strain } => {
                json!({
                    "path": path,
                    "gamemode": "mode_mania",
                    "title": title,
                    "username": user.username,
                    "flag": flag,
                    "gamemode_icon": "img/gamemodes/Mania.svg",
                    "user_pfp": user.avatar_url,
                    "accuracy_enabled": "show",
                    "accuracy": acc.trunc(),
                    "accuracy_decimal": (acc.fract() * 100.0).round() as u32,
                    "aim_enabled": "hidden",
                    "aim": 0.0,
                    "aim_decimal": 0.0,
                    "speed_enabled": "hidden",
                    "speed": 0.0,
                    "speed_decimal": 0.0,
                    "strain_enabled": "show",
                    "strain": strain.trunc(),
                    "strain_decimal": (strain.fract() * 100.0).round() as u32,
                    "movement_enabled": "hidden",
                    "movement": 0.0,
                    "movement_decimal": 0.0,
                    "global_rank": stats.global_rank.unwrap_or(0),
                    "country_rank": stats.country_rank.unwrap_or(0),
                    "level": stats.level.current,
                    "level_percentage": stats.level.progress,
                    "date": now,
                    "background_image": background,
                })
            }
        }
    }
}

struct Title {
    prefix: TitlePrefix,
    mods: ModDescriptions,
    main: TitleMain,
}

impl Display for Title {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.prefix)?;

        if !self.mods.is_empty() {
            write!(f, " {}", self.mods)?;
        }

        write!(f, " {}", self.main)
    }
}

impl Serialize for Title {
    #[inline]
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[derive(Debug)]
#[repr(u8)]
enum TitlePrefix {
    Newbie,
    Novice,
    Rookie,
    Apprentice,
    Advanced,
    Outstanding,
    Seasoned,
    Professional,
    Expert,
    Master,
    Legendary,
    God,
}

impl TitlePrefix {
    fn new(value: f64) -> Self {
        match value {
            _ if value < 10.0 => Self::Newbie,
            _ if value < 20.0 => Self::Novice,
            _ if value < 30.0 => Self::Rookie,
            _ if value < 40.0 => Self::Apprentice,
            _ if value < 50.0 => Self::Advanced,
            _ if value < 60.0 => Self::Outstanding,
            _ if value < 70.0 => Self::Seasoned,
            _ if value < 80.0 => Self::Professional,
            _ if value < 85.0 => Self::Expert,
            _ if value < 90.0 => Self::Master,
            _ if value < 95.0 => Self::Legendary,
            _ => Self::God,
        }
    }

    fn background(&self) -> &'static str {
        match self {
            Self::Newbie => "newbie",
            Self::Novice => "novice",
            Self::Rookie => "rookie",
            Self::Apprentice => "apprentice",
            Self::Advanced => "advanced",
            Self::Outstanding => "outstanding",
            Self::Seasoned => "seasoned",
            Self::Professional => "professional",
            Self::Expert => "expert",
            Self::Master => "master",
            Self::Legendary => "legendary",
            Self::God => "god",
        }
    }
}

impl Display for TitlePrefix {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        <Self as Debug>::fmt(self, f)
    }
}

enum ModDescription {
    ModHating,
    Speedy,
    AntClicking,
    HdAbusing,
    ModLoving,
    Versatile,
    Zooming,
    PeaCatching,
    GhostFruit,
    Key(usize),
    MultiKey,
}

impl Display for ModDescription {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let desc = match self {
            Self::ModHating => "Mod-Hating",
            Self::Speedy => "Speedy",
            Self::AntClicking => "Ant-Clicking",
            Self::HdAbusing => "HD-Abusing",
            Self::ModLoving => "Mod-Loving",
            Self::Versatile => "Versatile",
            Self::Zooming => "Zooming",
            Self::PeaCatching => "Pea-Catching",
            Self::GhostFruit => "Ghost-Fruit",
            Self::Key(key) => return write!(f, "{key}K"),
            Self::MultiKey => "Multi-Key",
        };

        f.write_str(desc)
    }
}

#[derive(Default)]
struct ModDescriptions(Vec<ModDescription>);

impl ModDescriptions {
    fn mania(scores: &[Score]) -> Self {
        let mut key_counts = [0_u8; 11];
        let mut doubletime = 0;

        for score in scores {
            doubletime += score.mods.contains(GameMods::DoubleTime) as usize;

            let idx = match score.mods.has_key_mod() {
                Some(GameMods::Key1) => 1,
                Some(GameMods::Key2) => 2,
                Some(GameMods::Key3) => 3,
                Some(GameMods::Key4) => 4,
                Some(GameMods::Key5) => 5,
                Some(GameMods::Key6) => 6,
                Some(GameMods::Key7) => 7,
                Some(GameMods::Key8) => 8,
                Some(GameMods::Key9) => 9,
                _ => score.map.as_ref().unwrap().cs.round() as usize,
            };

            key_counts[idx] += 1;
        }

        let mut mods = Self::default();

        if doubletime > 70 {
            mods.push(ModDescription::Speedy);
        }

        let (max, second_max, max_idx) = key_counts.into_iter().enumerate().skip(1).fold(
            (0, 0, 0),
            |(mut max, mut second_max, mut max_idx), (i, mut next)| {
                if next > max {
                    mem::swap(&mut max, &mut next);
                    max_idx = i;
                }

                if next > second_max {
                    mem::swap(&mut second_max, &mut next);
                }

                (max, second_max, max_idx)
            },
        );

        if max as f32 * 0.8 > second_max as f32 {
            mods.push(ModDescription::Key(max_idx));
        } else {
            mods.push(ModDescription::MultiKey);
        }

        mods
    }

    fn new(mode: GameMode, scores: &[Score]) -> Self {
        if mode == GameMode::Mania {
            return Self::mania(scores);
        }

        let mut nomod = 0;
        let mut hidden = 0;
        let mut doubletime = 0;
        let mut hardrock = 0;

        for score in scores {
            if score.mods.is_empty() {
                nomod += 1;
                continue;
            }

            hidden += score.mods.contains(GameMods::Hidden) as usize;
            doubletime += score.mods.contains(GameMods::DoubleTime) as usize;
            hardrock += score.mods.contains(GameMods::HardRock) as usize;
        }

        if nomod > 70 {
            return ModDescription::ModHating.into();
        }

        let mut mods = Self::default();

        if doubletime > 70 {
            mods.push(ModDescription::Speedy);
        }

        if hardrock > 70 {
            let desc = match mode {
                GameMode::Osu => ModDescription::AntClicking,
                GameMode::Taiko => ModDescription::Zooming,
                GameMode::Catch => ModDescription::PeaCatching,
                GameMode::Mania => unreachable!(),
            };

            mods.push(desc);
        }

        if hidden > 70 {
            let desc = match mode {
                GameMode::Osu | GameMode::Taiko => ModDescription::HdAbusing,
                GameMode::Catch => ModDescription::GhostFruit,
                GameMode::Mania => unreachable!(),
            };

            mods.push(desc);
        }

        if !mods.is_empty() {
            mods
        } else if nomod < 10 {
            ModDescription::ModLoving.into()
        } else {
            ModDescription::Versatile.into()
        }
    }

    fn push(&mut self, desc: ModDescription) {
        self.0.push(desc);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<ModDescription> for ModDescriptions {
    #[inline]
    fn from(desc: ModDescription) -> Self {
        Self(vec![desc])
    }
}

impl Display for ModDescriptions {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let mut iter = self.0.iter();

        if let Some(desc) = iter.next() {
            write!(f, "{desc}")?;

            for desc in iter {
                write!(f, " {desc}")?;
            }
        }

        Ok(())
    }
}

enum TitleMain {
    AllRounder,
    Sniper,
    Ninja,
    RhythmEnjoyer,
    Gunslinger,
    WhackAMole,
    Masher,
    Gamer,
    DropletDodger,
}

impl TitleMain {
    const THRESHOLD: f64 = 0.91;

    fn osu(acc: f64, aim: f64, speed: f64, max: f64) -> Self {
        let acc = Self::is_within_threshold(acc, max);
        let aim = Self::is_within_threshold(aim, max);
        let speed = Self::is_within_threshold(speed, max);

        match (acc, aim, speed) {
            (true, true, true) => Self::AllRounder,
            (true, true, false) => Self::Sniper,
            (true, false, true) => Self::Ninja,
            (true, false, false) => Self::RhythmEnjoyer,
            (false, true, true) => Self::Gunslinger,
            (false, true, false) => Self::WhackAMole,
            (false, false, true) => Self::Masher,
            (false, false, false) => unreachable!(),
        }
    }

    fn taiko(acc: f64, strain: f64, max: f64) -> Self {
        let acc = Self::is_within_threshold(acc, max);
        let strain = Self::is_within_threshold(strain, max);

        match (acc, strain) {
            (true, true) => Self::Gamer,
            (true, false) => Self::RhythmEnjoyer,
            (false, true) => Self::Masher,
            (false, false) => unreachable!(),
        }
    }

    fn catch(acc: f64, movement: f64, max: f64) -> Self {
        let acc = Self::is_within_threshold(acc, max);
        let movement = Self::is_within_threshold(movement, max);

        match (acc, movement) {
            (true, true) => Self::Gamer,
            (true, false) => Self::RhythmEnjoyer,
            (false, true) => Self::DropletDodger,
            (false, false) => unreachable!(),
        }
    }

    fn mania(acc: f64, strain: f64, max: f64) -> Self {
        let acc = Self::is_within_threshold(acc, max);
        let strain = Self::is_within_threshold(strain, max);

        match (acc, strain) {
            (true, true) => Self::Gamer,
            (true, false) => Self::RhythmEnjoyer,
            (false, true) => Self::Masher,
            (false, false) => unreachable!(),
        }
    }

    fn is_within_threshold(val: f64, max: f64) -> bool {
        val / max > Self::THRESHOLD
    }
}

impl Display for TitleMain {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let main = match self {
            Self::AllRounder => "All-Rounder",
            Self::Sniper => "Sniper",
            Self::Ninja => "Ninja",
            Self::RhythmEnjoyer => "Rhythm Enjoyer",
            Self::Gunslinger => "Gunslinger",
            Self::WhackAMole => "Whack-A-Mole",
            Self::Masher => "Masher",
            Self::Gamer => "Gamer",
            Self::DropletDodger => "Droplet Dodger",
        };

        f.write_str(main)
    }
}
