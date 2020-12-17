use crate::{bg_game::MapsetTags, embeds::EmbedData};

pub struct BGTagsEmbed {
    title: &'static str,
    description: Option<&'static str>,
    fields: Vec<(String, String, bool)>,
}

impl BGTagsEmbed {
    pub fn new(included: MapsetTags, excluded: MapsetTags, amount: usize) -> Self {
        let include_value = if !included.is_empty() {
            included.join("\n")
        } else if excluded.is_empty() {
            "Any".to_owned()
        } else {
            "None".to_owned()
        };
        let excluded_value = if !excluded.is_empty() {
            excluded.join("\n")
        } else {
            "None".to_owned()
        };
        let fields = vec![
            ("Included".to_owned(), include_value, true),
            ("Excluded".to_owned(), excluded_value, true),
            ("#Backgrounds".to_owned(), amount.to_string(), true),
        ];
        let description = if amount == 0 {
            Some("No stored backgrounds match these tags, try different ones")
        } else {
            None
        };
        Self {
            title: "Selected tags",
            fields,
            description,
        }
    }
}

impl EmbedData for BGTagsEmbed {
    fn title_owned(&mut self) -> Option<String> {
        Some(self.title.to_owned())
    }
    fn description_owned(&mut self) -> Option<String> {
        self.description.take().map(str::to_owned)
    }
    fn fields_owned(self) -> Option<Vec<(String, String, bool)>> {
        Some(self.fields)
    }
}
