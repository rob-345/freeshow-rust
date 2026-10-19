use crate::js::{self, Js};
use serde_json::Value;
use std::{collections::HashMap, sync::OnceLock};

pub(crate) fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("schema.json")).expect("checked music-metadata schema")
    })
}

pub(crate) struct Collector {
    pub common: Js,
    origin: HashMap<String, u16>,
    count: usize,
    limit: usize,
    pub exceeded: bool,
}
impl Collector {
    pub fn new(limit: usize) -> Self {
        let mut common = Js::object();
        for key in ["track", "disk"] {
            let mut pair = Js::object();
            pair.set("no", Js::Null);
            pair.set("of", Js::Null);
            common.set(key, pair);
        }
        common.set("movementIndex", Js::object());
        Self {
            common,
            origin: HashMap::new(),
            count: 0,
            limit,
            exceeded: false,
        }
    }
    pub fn add(&mut self, source: &str, id: &str, mut value: Js) {
        self.count += 1;
        if self.count > self.limit {
            self.exceeded = true;
            return;
        }
        if source == "vorbis" && id.starts_with("RATING:") {
            let mut rating = Js::object();
            rating.set(
                "source",
                Js::String(id.split(':').nth(1).unwrap_or("").to_lowercase()),
            );
            rating.set("rating", Js::Number(js::parse_float(&value.string())));
            self.mapped(3, "rating", rating);
            return;
        }
        let lookup = if matches!(source, "id3" | "mp4") {
            id.to_uppercase()
        } else {
            id.to_owned()
        };
        let Some(key) = schema()["maps"][source][&lookup].as_str() else {
            return;
        };
        if key == "picture" {
            return;
        }
        let priority = match source {
            "vorbis" => 3,
            "id3" => 4,
            "riff" => 7,
            "mp4" => 9,
            _ => 1000,
        };
        // Mapper-specific corrections happen before collector.postMap.
        if key == "rating" && matches!(source, "vorbis" | "mp4") {
            let mut rating = Js::object();
            rating.set(
                "rating",
                Js::Number(js::parse_float(&value.string()) / 100.0),
            );
            value = rating;
        }
        self.mapped(priority, key, value);
    }
    fn mapped(&mut self, priority: u16, key: &str, mut value: Js) {
        match key {
            "artist" => {
                if self.origin.get("artist") == Some(&priority) {
                    self.mapped(500, "artists", value);
                    return;
                }
                if matches!(self.common.get("artists"), Js::Undefined) {
                    self.set(500, "artists", value.clone());
                }
            }
            "artists" => {
                if !self.common.get("artist").truthy() || self.origin.get("artist") == Some(&500) {
                    let mut artists = match self.common.get("artists") {
                        Js::Array(values) => values.clone(),
                        _ => vec![],
                    };
                    if !artists
                        .iter()
                        .any(|artist| artist.string() == value.string())
                    {
                        artists.push(value.clone());
                        let artists: Vec<_> = artists.iter().map(Js::string).collect();
                        let name = if artists.len() > 2 {
                            format!(
                                "{} & {}",
                                artists[..artists.len() - 1].join(", "),
                                artists.last().unwrap()
                            )
                        } else {
                            artists.join(" & ")
                        };
                        self.set(500, "artist", Js::String(name));
                    }
                }
            }
            "totaltracks" | "totaldiscs" | "movementTotal" => {
                let pair = match key {
                    "totaltracks" => "track",
                    "totaldiscs" => "disk",
                    _ => "movementIndex",
                };
                let mut item = self.common.get(pair).clone();
                item.set("of", int_or_null(&value.string()));
                self.common.set(pair, item);
                return;
            }
            "track" | "disk" | "movementIndex" => {
                let existing = self.common.get(key).get("of").clone();
                let text = value.string();
                let mut parts = text.split('/');
                let mut pair = Js::object();
                for field in ["no", "of"] {
                    let number = js::parse_int(parts.next().unwrap_or(""));
                    pair.set(
                        field,
                        if number.is_nan() || number == 0.0 {
                            Js::Null
                        } else {
                            Js::Number(number)
                        },
                    );
                }
                if !existing.nullish() {
                    pair.set("of", existing);
                }
                self.common.set(key, pair);
                return;
            }
            "bpm" | "year" | "originalyear" => value = Js::Number(js::parse_int(&value.string())),
            "date" => {
                let text = value.string();
                let year = js::parse_int(&text.chars().take(4).collect::<String>());
                if !year.is_nan() {
                    self.common.set("year", Js::Number(year));
                }
            }
            "replaygain_track_gain"
            | "replaygain_track_peak"
            | "replaygain_album_gain"
            | "replaygain_album_peak" => {
                let text = value.string();
                let parts: Vec<_> = text
                    .split(' ')
                    .map(|s| s.trim_matches(js::whitespace).to_lowercase())
                    .collect();
                let number = js::parse_float(parts[0].as_str());
                let db = parts.len() == 2 && parts[1] == "db";
                let mut ratio = Js::object();
                ratio.set(
                    "dB",
                    Js::Number(if db { number } else { 10.0 * number.log10() }),
                );
                ratio.set(
                    "ratio",
                    Js::Number(if db {
                        10f64.powf(number / 10.0)
                    } else {
                        number
                    }),
                );
                value = ratio;
            }
            "replaygain_track_minmax" => {
                value = Js::Array(
                    value
                        .string()
                        .split(',')
                        .map(|s| Js::Number(js::parse_int(s)))
                        .collect(),
                )
            }
            "replaygain_undo" => {
                let text = value.string();
                let mut parts = text.split(',');
                let mut undo = Js::object();
                undo.set(
                    "leftChannel",
                    Js::Number(js::parse_int(parts.next().unwrap_or(""))),
                );
                if let Some(right) = parts.next() {
                    undo.set("rightChannel", Js::Number(js::parse_int(right)));
                }
                value = undo;
            }
            "gapless" | "compilation" | "podcast" | "showMovement" => {
                value = Js::Bool(
                    matches!(&value,Js::String(text) if text=="1")
                        || matches!(value,Js::Number(number) if number==1.0),
                )
            }
            "discogs_label_id"
            | "discogs_release_id"
            | "discogs_master_release_id"
            | "discogs_artist_id"
            | "discogs_votes"
                if matches!(value, Js::String(_)) =>
            {
                value = Js::Number(js::parse_int(&value.string()))
            }
            _ => {}
        }
        if !value.is_null() {
            self.set(priority, key, value);
        }
    }
    fn set(&mut self, priority: u16, key: &str, value: Js) {
        let old = *self.origin.get(key).unwrap_or(&1000);
        let spec = &schema()["common"][key];
        let multiple = spec["multiple"].as_bool().unwrap_or(false);
        if !multiple {
            if priority <= old {
                self.common.set(key, value);
                self.origin.insert(key.into(), priority);
            }
        } else if priority == old {
            let mut values = match self.common.get(key) {
                Js::Array(values) => values.clone(),
                _ => vec![],
            };
            if key != "isrc" && spec["unique"] != true
                || !values.iter().any(|current| current.json() == value.json())
            {
                values.push(value);
                self.common.set(key, Js::Array(values));
            }
        } else if priority < old {
            self.common.set(key, Js::Array(vec![value]));
            self.origin.insert(key.into(), priority);
        }
    }
}
fn int_or_null(value: &str) -> Js {
    let n = js::parse_int(value);
    if n.is_nan() {
        Js::Null
    } else {
        Js::Number(n)
    }
}

pub(crate) fn genre(text: &str) -> Vec<Js> {
    fn code(text: &str) -> Option<Js> {
        match text {
            "RX" => Some("Remix".into()),
            "CR" => Some("Cover".into()),
            _ => text
                .parse::<usize>()
                .ok()
                .and_then(|index| schema()["genres"][index].as_str().map(Js::from)),
        }
    }
    let mut genres = vec![];
    let mut pending: Option<String> = None;
    let mut word = String::new();
    for c in text.chars() {
        if let Some(p) = &mut pending {
            if c == '(' && p.is_empty() {
                word.push('(');
                pending = None;
            } else if c == ')' {
                if !word.is_empty() {
                    genres.push(Js::String(std::mem::take(&mut word)));
                }
                if let Some(value) = code(p) {
                    genres.push(value);
                }
                pending = None;
            } else {
                p.push(c);
            }
        } else if c == '(' {
            pending = Some(String::new());
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        genres.push(
            if genres.is_empty() && word.chars().all(|c| c.is_ascii_digit()) {
                code(&word).unwrap_or(Js::Undefined)
            } else {
                Js::String(word)
            },
        );
    }
    genres
}
