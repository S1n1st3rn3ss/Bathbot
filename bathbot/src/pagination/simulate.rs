use twilight_model::channel::embed::Embed;

use crate::{
    embeds::{EmbedData, SimulateData, SimulateEmbed},
    manager::OsuMap,
};

use super::{Pages, PaginationBuilder, PaginationKind};

// Not using #[pagination(...)] since it requires special initialization
pub struct SimulatePagination {
    map: OsuMap,
    pub simulate_data: SimulateData,
}

impl SimulatePagination {
    pub fn builder(map: OsuMap, simulate_data: SimulateData) -> PaginationBuilder {
        // initialization doesn't really matter since the index is always set manually anyway
        let pages = Pages::new(1, usize::MAX);

        let pagination = Self { map, simulate_data };
        let kind = PaginationKind::Simulate(Box::new(pagination));

        PaginationBuilder::new(kind, pages)
    }

    pub fn build_page(&mut self) -> Embed {
        if let Some(ar) = self.simulate_data.ar {
            self.map.pp_map.ar = ar;
        }

        if let Some(cs) = self.simulate_data.cs {
            self.map.pp_map.cs = cs;
        }

        if let Some(hp) = self.simulate_data.hp {
            self.map.pp_map.hp = hp;
        }

        if let Some(od) = self.simulate_data.od {
            self.map.pp_map.od = od;
        }

        SimulateEmbed::new(&self.map, &self.simulate_data).build()
    }
}