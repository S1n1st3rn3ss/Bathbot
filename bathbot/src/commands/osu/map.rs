use std::{
    borrow::Cow, cell::RefCell, cmp::Ordering, fmt::Write, iter, mem, rc::Rc,
    result::Result as StdResult, slice, sync::Arc, time::Duration,
};

use bathbot_macros::{command, HasMods, SlashCommand};
use bathbot_util::{
    constants::{GENERAL_ISSUE, OSU_API_ISSUE},
    matcher,
    osu::MapIdType,
};
use enterpolation::{linear::Linear, Curve};
use eyre::{Report, Result, WrapErr};
use image::{codecs::png::PngEncoder, ColorType, DynamicImage, ImageEncoder};
use plotters::{
    backend::{PixelFormat, RGBPixel},
    coord::{types::RangedCoordf64, Shift},
    prelude::*,
};
use plotters_backend::{BackendColor, BackendCoord, DrawingErrorKind};
use rosu_pp::{BeatmapExt, Strains};
use rosu_v2::prelude::{GameMode, GameMods, OsuError};
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    channel::{message::MessageType, Message},
    guild::Permissions,
};

use crate::{
    core::commands::{prefix::Args, CommandOrigin},
    embeds::MessageOrigin,
    pagination::MapPagination,
    util::{interaction::InteractionCommand, ChannelExt, InteractionCommandExt},
    Context,
};

use super::{HasMods, ModsResult};

#[derive(CommandModel, CreateCommand, SlashCommand)]
#[command(
    name = "map",
    help = "Display a bunch of stats about a map(set).\n\
    The values in the map info will be adjusted to mods.\n\
    Since discord does not allow images to be adjusted when editing messages, \
    the strain graph always belongs to the initial map, even after moving to \
    other maps of the set through the pagination buttons."
)]
/// Display a bunch of stats about a map(set)
pub struct Map<'a> {
    #[command(help = "Specify a map either by map url or map id.\n\
    If none is specified, it will search in the recent channel history \
    and pick the first map it can find.")]
    /// Specify a map url or map id
    map: Option<Cow<'a, str>>,
    #[command(
        help = "Specify mods either directly or through the explicit `+mods!` / `+mods` syntax e.g. `hdhr` or `+hdhr!`"
    )]
    /// Specify mods e.g. hdhr or nm
    mods: Option<Cow<'a, str>>,
    #[command(min_value = 0.0, max_value = 10.0)]
    /// Specify an AR value to override the actual one
    ar: Option<f64>,
    #[command(min_value = 0.0, max_value = 10.0)]
    /// Specify an OD value to override the actual one
    od: Option<f64>,
    #[command(min_value = 0.0, max_value = 10.0)]
    /// Specify a CS value to override the actual one
    cs: Option<f64>,
    #[command(min_value = 0.0, max_value = 10.0)]
    /// Specify an HP value to override the actual one
    hp: Option<f64>,
}

#[derive(HasMods)]
struct MapArgs<'a> {
    map: Option<MapIdType>,
    mods: Option<Cow<'a, str>>,
    attrs: CustomAttrs,
}

#[derive(Default)]
pub struct CustomAttrs {
    pub ar: Option<f64>,
    pub cs: Option<f64>,
    pub hp: Option<f64>,
    pub od: Option<f64>,
}

impl CustomAttrs {
    fn content(&self) -> Option<String> {
        self.ar.or(self.cs).or(self.hp).or(self.od)?;

        let mut content = "Custom attributes: ".to_owned();
        let mut pushed = false;

        if let Some(ar) = self.ar {
            let _ = write!(content, "`AR: {ar:.2}`");
            pushed = true;
        }

        if let Some(cs) = self.cs {
            if pushed {
                content.push_str(" ~ ");
            }

            let _ = write!(content, "`CS: {cs:.2}`");
            pushed = true;
        }

        if let Some(hp) = self.hp {
            if pushed {
                content.push_str(" ~ ");
            }

            let _ = write!(content, "`HP: {hp:.2}`");
            pushed = true;
        }

        if let Some(od) = self.od {
            if pushed {
                content.push_str(" ~ ");
            }

            let _ = write!(content, "`OD: {od:.2}`");
        }

        Some(content)
    }
}

impl<'m> MapArgs<'m> {
    fn args(msg: &Message, args: Args<'m>) -> Result<Self, String> {
        let mut map = None;
        let mut mods = None;

        for arg in args.take(2) {
            if let Some(id) = matcher::get_osu_map_id(arg)
                .map(MapIdType::Map)
                .or_else(|| matcher::get_osu_mapset_id(arg).map(MapIdType::Set))
            {
                map = Some(id);
            } else if matcher::get_mods(arg).is_some() {
                mods = Some(arg.into());
            } else {
                let content = format!(
                    "Failed to parse `{arg}`.\n\
                    Be sure you specify either a valid map id, map url, or mod combination."
                );

                return Err(content);
            }
        }

        let reply = msg
            .referenced_message
            .as_deref()
            .filter(|_| msg.kind == MessageType::Reply);

        if let Some(id) = reply.and_then(MapIdType::from_msg) {
            map = Some(id);
        }

        Ok(Self {
            map,
            mods,
            attrs: CustomAttrs::default(),
        })
    }
}

impl<'a> TryFrom<Map<'a>> for MapArgs<'a> {
    type Error = &'static str;

    fn try_from(args: Map<'a>) -> Result<Self, Self::Error> {
        let Map {
            map,
            mods,
            ar,
            od,
            cs,
            hp,
        } = args;

        let map = match map.map(|arg| {
            matcher::get_osu_map_id(&arg)
                .map(MapIdType::Map)
                .or_else(|| matcher::get_osu_mapset_id(&arg).map(MapIdType::Set))
        }) {
            Some(Some(id)) => Some(id),
            Some(None) => {
                let content =
                    "Failed to parse map url. Be sure you specify a valid map id or url to a map.";

                return Err(content);
            }
            None => None,
        };

        let attrs = CustomAttrs { ar, cs, hp, od };

        Ok(Self { map, mods, attrs })
    }
}

#[command]
#[desc("Display a bunch of stats about a map(set)")]
#[help(
    "Display stats about a beatmap. Mods can be specified.\n\
    If no map(set) is specified by either url or id, I will choose the last map \
    I can find in the embeds of this channel.\n\
    If the mapset is specified by id but there is some map with the same id, \
    I will choose the latter."
)]
#[usage("[map(set) url / map(set) id] [+mods]")]
#[examples("2240404 +hddt", "https://osu.ppy.sh/beatmapsets/902425 +hr")]
#[aliases("m", "beatmap", "maps", "beatmaps", "mapinfo")]
#[group(AllModes)]
async fn prefix_map(
    ctx: Arc<Context>,
    msg: &Message,
    args: Args<'_>,
    permissions: Option<Permissions>,
) -> Result<()> {
    match MapArgs::args(msg, args) {
        Ok(args) => map(ctx, CommandOrigin::from_msg(msg, permissions), args).await,
        Err(content) => {
            msg.error(&ctx, content).await?;

            Ok(())
        }
    }
}

async fn slash_map(ctx: Arc<Context>, mut command: InteractionCommand) -> Result<()> {
    let args = Map::from_interaction(command.input_data())?;

    match MapArgs::try_from(args) {
        Ok(args) => map(ctx, (&mut command).into(), args).await,
        Err(content) => {
            command.error(&ctx, content).await?;

            Ok(())
        }
    }
}

const W: u32 = 590;
const H: u32 = 150;

async fn map(ctx: Arc<Context>, orig: CommandOrigin<'_>, args: MapArgs<'_>) -> Result<()> {
    let mods = match args.mods() {
        ModsResult::Mods(mods) => Some(mods),
        ModsResult::None => None,
        ModsResult::Invalid => {
            let content =
                "Failed to parse mods. Be sure to specify a valid abbreviation e.g. `hdhr`.";

            return orig.error(&ctx, content).await;
        }
    };

    let MapArgs { map, attrs, .. } = args;

    let map_id = if let Some(id) = map {
        id
    } else if orig.can_read_history() {
        let msgs = match ctx.retrieve_channel_history(orig.channel_id()).await {
            Ok(msgs) => msgs,
            Err(err) => {
                let _ = orig.error(&ctx, GENERAL_ISSUE).await;

                return Err(err.wrap_err("failed to retrieve channel history"));
            }
        };

        match MapIdType::from_msgs(&msgs, 0) {
            Some(id) => id,
            None => {
                let content = "No beatmap specified and none found in recent channel history. \
                    Try specifying a map(set) either by url to the map, \
                    or just by map(set) id.";

                return orig.error(&ctx, content).await;
            }
        }
    } else {
        let content =
            "No beatmap specified and lacking permission to search the channel history for maps.\n\
            Try specifying a map(set) either by url to the map, \
            or just by map(set) id, or give me the \"Read Message History\" permission.";

        return orig.error(&ctx, content).await;
    };

    let mods = match mods {
        Some(selection) => selection.mods(),
        None => GameMods::NoMod,
    };

    let mapset_res = match map_id {
        MapIdType::Map(id) => ctx.osu().beatmapset_from_map_id(id).await,
        MapIdType::Set(id) => ctx.osu().beatmapset(id).await,
    };

    let mut mapset = match mapset_res {
        Ok(mapset) => mapset,
        Err(OsuError::NotFound) => {
            let content = match map_id {
                MapIdType::Map(id) => format!("Beatmapset of map {id} was not found"),
                MapIdType::Set(id) => format!("Beatmapset with id {id} was not found"),
            };

            return orig.error(&ctx, content).await;
        }
        Err(err) => {
            let _ = orig.error(&ctx, OSU_API_ISSUE).await;

            return Err(Report::new(err).wrap_err("failed to get mapset"));
        }
    };

    if let Err(err) = ctx.osu_map().store(&mapset).await {
        warn!("{err:?}");
    }

    let Some(mut maps) = mapset.maps.take().filter(|maps| !maps.is_empty()) else {
        return orig.error(&ctx, "The mapset has no maps").await;
    };

    maps.sort_unstable_by(|m1, m2| {
        m1.mode.cmp(&m2.mode).then_with(|| match m1.mode {
            // For mania sort first by mania key, then star rating
            GameMode::Mania => m1
                .cs
                .partial_cmp(&m2.cs)
                .unwrap_or(Ordering::Equal)
                .then(m1.stars.partial_cmp(&m2.stars).unwrap_or(Ordering::Equal)),
            // For other mods just sort by star rating
            _ => m1.stars.partial_cmp(&m2.stars).unwrap_or(Ordering::Equal),
        })
    });

    let map_idx = match map_id {
        MapIdType::Map(map_id) => maps
            .iter()
            .position(|map| map.map_id == map_id)
            .unwrap_or(0),
        MapIdType::Set(_) => 0,
    };

    let map_id = maps[map_idx].map_id;

    // Try creating the strain graph for the map
    let bg_fut = async {
        let bytes = ctx.client().get_mapset_cover(&mapset.covers.cover).await?;

        let cover =
            image::load_from_memory(&bytes).wrap_err("failed to load mapset cover from memory")?;

        Ok::<_, Report>(cover.thumbnail_exact(W, H))
    };

    let (strain_values_res, img_res) = tokio::join!(strain_values(&ctx, map_id, mods), bg_fut);

    let img_opt = match img_res {
        Ok(img) => Some(img),
        Err(err) => {
            warn!("{:?}", err.wrap_err("Failed to get graph background"));

            None
        }
    };

    let graph = match strain_values_res {
        Ok(strain_values) => match graph(strain_values, img_opt) {
            Ok(graph) => Some(graph),
            Err(err) => {
                warn!("{:?}", err.wrap_err("Failed to create graph"));

                None
            }
        },
        Err(err) => {
            warn!("{:?}", err.wrap_err("Failed to calculate strain values"));

            None
        }
    };

    let content = attrs.content();

    let origin = MessageOrigin::new(orig.guild_id(), orig.channel_id());
    let mut builder = MapPagination::builder(mapset, maps, mods, map_idx, attrs, origin);

    if let Some(bytes) = graph {
        builder = builder.attachment("map_graph.png", bytes);
    }

    if let Some(content) = content {
        builder = builder.content(content);
    }

    builder
        .start_by_update()
        .defer_components()
        .start(ctx, orig)
        .await
}

struct GraphStrains {
    strains: Strains,
    strains_count: usize,
}

const NEW_STRAIN_COUNT: usize = 128;

async fn strain_values(ctx: &Context, map_id: u32, mods: GameMods) -> Result<GraphStrains> {
    let map = ctx
        .osu_map()
        .pp_map(map_id)
        .await
        .wrap_err("failed to get pp map")?;

    let mut strains = map.strains(mods.bits());
    let section_len = strains.section_len();
    let strains_count = strains.len();

    let create_curve = |strains: Vec<f64>| {
        Linear::builder()
            .elements(strains)
            .equidistant()
            .distance(0.0, section_len)
            .build()
            .map(|curve| curve.take(NEW_STRAIN_COUNT).collect())
    };

    match &mut strains {
        Strains::Osu(strains) => {
            strains
                .aim
                .iter()
                .zip(strains.aim_no_sliders.iter_mut())
                .for_each(|(aim, no_slider)| *no_slider = *aim - *no_slider);

            strains.aim =
                create_curve(mem::take(&mut strains.aim)).wrap_err("Failed to build aim curve")?;
            strains.aim_no_sliders = create_curve(mem::take(&mut strains.aim_no_sliders))
                .wrap_err("Failed to build aim_no_sliders curve")?;
            strains.speed = create_curve(mem::take(&mut strains.speed))
                .wrap_err("Failed to build speed curve")?;
            strains.flashlight = create_curve(mem::take(&mut strains.flashlight))
                .wrap_err("Failed to build flashlight curve")?;
        }
        Strains::Taiko(strains) => {
            strains.color = create_curve(mem::take(&mut strains.color))
                .wrap_err("Failed to build color curve")?;
            strains.rhythm = create_curve(mem::take(&mut strains.rhythm))
                .wrap_err("Failed to build rhythm curve")?;
            strains.stamina = create_curve(mem::take(&mut strains.stamina))
                .wrap_err("Failed to build stamina curve")?;
        }
        Strains::Catch(strains) => {
            strains.movement = create_curve(mem::take(&mut strains.movement))
                .wrap_err("Failed to build movement curve")?;
        }
        Strains::Mania(strains) => {
            strains.strains = create_curve(mem::take(&mut strains.strains))
                .wrap_err("Failed to build strains curve")?;
        }
    }

    Ok(GraphStrains {
        strains,
        strains_count,
    })
}

fn graph(strains: GraphStrains, background: Option<DynamicImage>) -> Result<Vec<u8>> {
    let last_timestamp = ((NEW_STRAIN_COUNT - 1) as f64
        * strains.strains.section_len()
        * strains.strains_count as f64)
        / NEW_STRAIN_COUNT as f64;

    let max_strain = match &strains.strains {
        Strains::Osu(strains) => strains
            .aim
            .iter()
            .zip(strains.aim_no_sliders.iter())
            .zip(strains.speed.iter())
            .zip(strains.flashlight.iter())
            .fold(0.0_f64, |max, (((a, b), c), d)| {
                max.max(*a).max(*b).max(*c).max(*d)
            }),
        Strains::Taiko(strains) => strains
            .color
            .iter()
            .zip(strains.rhythm.iter())
            .zip(strains.stamina.iter())
            .fold(0.0_f64, |max, ((a, b), c)| max.max(*a).max(*b).max(*c)),
        Strains::Catch(strains) => strains
            .movement
            .iter()
            .fold(0.0_f64, |max, strain| max.max(*strain)),
        Strains::Mania(strains) => strains
            .strains
            .iter()
            .fold(0.0_f64, |max, strain| max.max(*strain)),
    };

    if max_strain <= std::f64::EPSILON {
        bail!("no non-zero strain point");
    }

    let mut buf = vec![0; (3 * W * H) as usize];

    let buf_ptr = buf.as_ptr();

    {
        let backend = Rc::new(RefCell::new(BlendableBackend::new(&mut buf, buf_ptr, W, H)));
        let root = BlendableBackend::to_drawing_area(&backend);

        if background.is_none() {
            root.fill(&RGBColor(19, 43, 33))
                .wrap_err("Failed to fill background")?;
        }

        let mut chart = ChartBuilder::on(&root)
            .x_label_area_size(17_i32)
            .build_cartesian_2d(0.0_f64..last_timestamp, 0.0_f64..max_strain)
            .wrap_err("Failed to build chart")?;

        // Add background
        if let Some(background) = background {
            let background = background.blur(2.0);
            let elem: BitMapElement<'_, _> = ((0.0_f64, max_strain), background).into();
            chart
                .draw_series(iter::once(elem))
                .wrap_err("Failed to draw background")?;

            let rect = Rectangle::new([(0, 0), (W as i32, H as i32)], BLACK.mix(0.8).filled());
            root.draw(&rect)
                .wrap_err("Failed to draw darkening rectangle")?;
        }

        // Mesh and labels
        let text_style = FontDesc::new(FontFamily::SansSerif, 14.0, FontStyle::Bold).color(&WHITE);

        chart
            .configure_mesh()
            .disable_y_mesh()
            .disable_y_axis()
            .set_all_tick_mark_size(3_i32)
            .light_line_style(WHITE.mix(0.0)) // hide
            .bold_line_style(WHITE.mix(0.4))
            .x_labels(10)
            .x_label_style(text_style.clone())
            .axis_style(WHITE)
            .x_label_formatter(&|timestamp| {
                if timestamp.abs() <= f64::EPSILON {
                    return String::new();
                }

                let d = Duration::from_millis(*timestamp as u64);
                let minutes = d.as_secs() / 60;
                let seconds = d.as_secs() % 60;

                format!("{minutes}:{seconds:0>2}")
            })
            .draw()
            .wrap_err("Failed to draw mesh")?;

        backend.borrow_mut().toggle_blending();
        draw_mode_strains(&mut chart, strains)?;
        backend.borrow_mut().toggle_blending();

        chart
            .configure_series_labels()
            .position(SeriesLabelPosition::UpperLeft)
            .border_style(BLACK.mix(0.25))
            .background_style(BLACK.mix(0.15))
            .margin(2_i32)
            .legend_area_size(10)
            .label_font(text_style)
            .draw()
            .wrap_err("Failed to draw legend")?;
    }

    // Encode buf to png
    let mut png_bytes: Vec<u8> = Vec::with_capacity((2 * W * H) as usize);

    PngEncoder::new(&mut png_bytes)
        .write_image(&buf, W, H, ColorType::Rgb8)
        .wrap_err("Failed to encode image")?;

    Ok(png_bytes)
}

fn draw_mode_strains(
    chart: &mut ChartContext<'_, BlendableBackend<'_>, Cartesian2d<RangedCoordf64, RangedCoordf64>>,
    strains: GraphStrains,
) -> Result<()> {
    let GraphStrains {
        strains,
        strains_count,
    } = strains;

    let orig_count = strains_count as f64;
    let new_count = strains.len() as f64;
    let section_len = strains.section_len();

    let factor = section_len * orig_count / new_count;

    fn timestamp_iter(strains: &[f64], factor: f64) -> impl Iterator<Item = (f64, f64)> + '_ {
        strains
            .iter()
            .enumerate()
            .map(move |(i, strain)| (i as f64 * factor, *strain))
    }

    macro_rules! draw_line {
        ( $label:literal, $strains:ident.$skill:ident, $color:ident ) => {{
            chart
                .draw_series(
                    AreaSeries::new(
                        timestamp_iter(&$strains.$skill, factor),
                        0.0,
                        $color.mix(0.3),
                    )
                    .border_style($color.stroke_width(1)),
                )
                .wrap_err(concat!("Failed to draw ", stringify!($skill), " series"))?
                .label($label)
                .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 6, y)], $color));
        }};
    }

    match strains {
        Strains::Osu(strains) => {
            draw_line!("Aim", strains.aim, CYAN);
            draw_line!("Aim (Sliders)", strains.aim_no_sliders, GREEN);
            draw_line!("Speed", strains.speed, RED);
            draw_line!("Flashlight", strains.flashlight, MAGENTA);
        }
        Strains::Taiko(strains) => {
            draw_line!("Stamina", strains.stamina, RED);
            draw_line!("Color", strains.color, YELLOW);
            draw_line!("Rhythm", strains.rhythm, CYAN);
        }
        Strains::Catch(strains) => draw_line!("Movement", strains.movement, CYAN),
        Strains::Mania(strains) => draw_line!("Strain", strains.strains, MAGENTA),
    }

    Ok(())
}

struct BlendableBackend<'a> {
    inner: BitMapBackend<'a>,
    // The inner BitMapBackend contains the pixel buffer but unfortunately
    // doesn't expose it so in order to still access the pixels we use this
    // pointer. That means at some points we have both a mutable and immutable
    // reference to the buffer which in general is unsound. YOLO :^)
    ptr: *const u8,
    blend: bool,
}

impl<'a> BlendableBackend<'a> {
    fn new(buf: &'a mut [u8], ptr: *const u8, width: u32, height: u32) -> Self {
        let inner = BitMapBackend::with_buffer(buf, (width, height));
        let blend = false;

        Self { inner, ptr, blend }
    }

    fn toggle_blending(&mut self) {
        self.blend = !self.blend;
    }

    fn to_drawing_area(this: &Rc<RefCell<Self>>) -> DrawingArea<Self, Shift> {
        DrawingArea::from(this)
    }
}

impl<'a> DrawingBackend for BlendableBackend<'a> {
    type ErrorType = <BitMapBackend<'a> as DrawingBackend>::ErrorType;

    #[inline]
    fn get_size(&self) -> (u32, u32) {
        self.inner.get_size()
    }

    #[inline]
    fn ensure_prepared(&mut self) -> StdResult<(), DrawingErrorKind<Self::ErrorType>> {
        self.inner.ensure_prepared()
    }

    #[inline]
    fn present(&mut self) -> StdResult<(), DrawingErrorKind<Self::ErrorType>> {
        self.inner.present()
    }

    fn draw_pixel(
        &mut self,
        point: BackendCoord,
        color: BackendColor,
    ) -> StdResult<(), DrawingErrorKind<Self::ErrorType>> {
        if !self.blend {
            return self.inner.draw_pixel(point, color);
        }

        // https://api.skia.org/SkBlendMode_8h.html#ad96d76accb8ff5f3eafa29b91f7a25f0
        fn blend_lighten(src: (u8, u8, u8), dst: BackendColor) -> BackendColor {
            let src_r = src.0 as f64 / 256.0;
            let src_g = src.1 as f64 / 256.0;
            let src_b = src.2 as f64 / 256.0;

            let dst_r = dst.rgb.0 as f64 / 256.0;
            let dst_g = dst.rgb.1 as f64 / 256.0;
            let dst_b = dst.rgb.2 as f64 / 256.0;

            let rc_r = src_r + dst_r;
            let rc_g = src_g + dst_g;
            let rc_b = src_b + dst_b;

            BackendColor {
                alpha: dst.alpha,
                rgb: (
                    (rc_r * 256.0) as u8,
                    (rc_g * 256.0) as u8,
                    (rc_b * 256.0) as u8,
                ),
            }
        }

        let (x, y) = (point.0 as usize, point.1 as usize);
        let (w, h) = self.inner.get_size();
        let w = w as usize;
        let h = h as usize;

        if x >= w || y >= h {
            return Ok(());
        }

        let offset = (y * w + x) * RGBPixel::PIXEL_SIZE;

        let len = w * h * RGBPixel::PIXEL_SIZE;
        let data = unsafe { slice::from_raw_parts(self.ptr.add(offset), len) };

        let (r, g, b, _) = RGBPixel::decode_pixel(data);
        let src = (r, g, b);
        let blended = blend_lighten(src, color);

        self.inner.draw_pixel(point, blended)
    }
}
