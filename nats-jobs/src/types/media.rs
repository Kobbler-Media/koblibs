use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported media kinds for download requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Book,
    Audiobook,
    TvEpisode,
    TvSeason,
    Movie,
    Comic,
    Music,
    Game,
    Software,
    SportEvent,
    SportSeason,
}

/// Per-request quality overrides that take precedence over the quality profile.
///
/// All fields are optional — only set fields override the profile defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QualityOverrides {
    pub min_resolution: Option<String>,
    pub max_resolution: Option<String>,
    pub source: Option<String>,
    pub codec: Option<String>,
    pub audio: Option<String>,
    pub preferred_languages: Option<Vec<String>>,
    pub rejected_languages: Option<Vec<String>>,
}

/// External identifiers for matching books across metadata providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BookIdentifiers {
    pub isbn10: Option<String>,
    pub isbn13: Option<String>,
    pub hardcover_id: Option<String>,
    pub goodreads_id: Option<String>,
    pub openlibrary_id: Option<String>,
}

/// External identifiers for matching TV shows across metadata providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TvIdentifiers {
    pub tmdb_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub imdb_id: Option<String>,
}

/// External identifiers for matching movies across metadata providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MovieIdentifiers {
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
}

/// Flattened into the event, so the wire format stays flat.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SportEventMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// ISO 3166-1 alpha-2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_iso: Option<String>,
    /// Each alias is a separate place name and must be matched on its own:
    /// joined, "Emilia Romagna Misano" no longer word-matches `Italy.Misano`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub league_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teams: Option<Vec<SportTeam>>,
    /// RFC 3339 instant the session starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifiers: Option<SportIdentifiers>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SportTeam {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SportIdentifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thesportsdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sportarr_id: Option<String>,
    /// The organiser's own id for the event (the MotoGP event uuid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_id: Option<String>,
}

/// A reference to a specific episode within a season.
///
/// Uses strings throughout to support daily shows ("2024-01-15"),
/// specials ("Specials"), year-based seasons, miniseries parts, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EpisodeRef {
    pub season: String,
    pub episode: String,
    /// For ordering when the identifier isn't naturally sortable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i32>,
    /// Air date of the episode (ISO 8601, e.g. "2025-03-15").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_date: Option<String>,
    /// Whether this episode has a file in the library.
    /// `Some(true)` = file exists, `Some(false)` = no file, `None` = unknown (old payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<bool>,
    /// Video height of the best library file (e.g. 1080, 2160).
    /// `None` when not in library or file exists but hasn't been probed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_video_height: Option<i32>,
    /// Episode title (e.g. "My First Day") for disambiguation boosting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Structured media request (preferred over raw patterns for normal flows).
///
/// Each variant generates search terms automatically and carries optional
/// external identifiers for precise matching against Newznab-compatible indexers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaWant {
    Book {
        title: String,
        author: Option<String>,
        isbn: Option<String>,
        #[serde(default)]
        format_preferences: Vec<String>,
        identifiers: Option<BookIdentifiers>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_position: Option<String>,
        /// Publication date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alternate_titles: Option<Vec<String>>,
    },
    Audiobook {
        title: String,
        author: Option<String>,
        narrator: Option<String>,
        isbn: Option<String>,
        #[serde(default)]
        format_preferences: Vec<String>,
        identifiers: Option<BookIdentifiers>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_position: Option<String>,
        /// Publication date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alternate_titles: Option<Vec<String>>,
    },
    TvEpisode {
        show_name: String,
        /// Season identifier as string (e.g. "1", "Specials", "2024").
        season: String,
        /// Episode identifier as string (e.g. "5", "2024-01-15", "Part 1").
        episode: String,
        year: Option<u32>,
        /// Air date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        air_date: Option<String>,
        identifiers: Option<TvIdentifiers>,
        /// Alternate show names from TMDB/TVDB (e.g. "CSI" for "CSI: Crime Scene Investigation").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alternate_titles: Option<Vec<String>>,
        /// Episode title (e.g. "My First Day") for disambiguation boosting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episode_title: Option<String>,
        quality_profile_id: Option<Uuid>,
        quality_overrides: Option<QualityOverrides>,
    },
    TvSeason {
        show_name: String,
        /// Season identifier as string (e.g. "1", "Specials", "2024").
        season: String,
        year: Option<u32>,
        /// Specific episodes still needed. When set, Kobload can fall back
        /// to per-episode search if no season pack is found.
        #[serde(default)]
        episodes_needed: Option<Vec<EpisodeRef>>,
        /// Total episode count for the season (from metadata provider).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_episodes: Option<u32>,
        /// Controls season pack preference: "never" (default), "auto" (ratio-based), "always".
        #[serde(default = "default_pack_preference")]
        pack_preference: String,
        /// Most recent episode air date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_air_date: Option<String>,
        identifiers: Option<TvIdentifiers>,
        /// Alternate show names from TMDB/TVDB (e.g. "CSI" for "CSI: Crime Scene Investigation").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alternate_titles: Option<Vec<String>>,
        quality_profile_id: Option<Uuid>,
        quality_overrides: Option<QualityOverrides>,
    },
    Movie {
        title: String,
        year: Option<u32>,
        /// Release date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_date: Option<String>,
        identifiers: Option<MovieIdentifiers>,
        /// Alternate movie titles from TMDB (e.g. regional or scene names).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alternate_titles: Option<Vec<String>>,
        quality_profile_id: Option<Uuid>,
        quality_overrides: Option<QualityOverrides>,
        /// Whether a file exists in the library for this movie.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_library: Option<bool>,
        /// Video height of the best library file (e.g. 1080, 2160).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_video_height: Option<i32>,
    },
    Comic {
        title: String,
        issue: Option<u32>,
        series: Option<String>,
        year: Option<u32>,
    },
    Music {
        artist: String,
        album: Option<String>,
        year: Option<u32>,
    },
    Game {
        title: String,
        platform: Option<String>,
        year: Option<u32>,
    },
    Software {
        name: String,
        version: Option<String>,
        platform: Option<String>,
    },
    SportEvent {
        league_name: String,
        /// Season year as string (e.g. "2025").
        season: String,
        /// Round number within the season (optional for non-motorsport).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        round: Option<u32>,
        /// Location/venue name (e.g. "Thailand", "Qatar"). Optional for team/combat sports.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        /// Session type (e.g. "Race", "Sprint", "Qualifying"). Optional for team/combat sports.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_type: Option<String>,
        /// Raw event title from TheSportsDB (e.g. "Arsenal vs Chelsea", "UFC 302 Main Card").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_title: Option<String>,
        /// Event date as ISO 8601 string (e.g. "2024-03-15").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_date: Option<String>,
        quality_profile_id: Option<Uuid>,
        quality_overrides: Option<QualityOverrides>,
        /// Whether a file exists in the library for this event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_library: Option<bool>,
        /// Video height of the best library file (e.g. 1080, 2160).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_video_height: Option<i32>,
        #[serde(flatten)]
        meta: Box<SportEventMeta>,
    },
    SportSeason {
        league_name: String,
        /// Season year as string (e.g. "2025").
        season: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        league_aliases: Option<Vec<String>>,
        quality_profile_id: Option<Uuid>,
        quality_overrides: Option<QualityOverrides>,
    },
}

fn default_pack_preference() -> String {
    "never".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tv_season_backward_compat_missing_new_fields() {
        let json = r#"{
            "kind": "tv_season",
            "show_name": "Test Show",
            "season": "1",
            "year": 2024
        }"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::TvSeason {
                total_episodes,
                pack_preference,
                last_air_date,
                ..
            } => {
                assert_eq!(total_episodes, None);
                assert_eq!(pack_preference, "never");
                assert_eq!(last_air_date, None);
            }
            _ => panic!("expected TvSeason"),
        }
    }

    #[test]
    fn test_tv_season_roundtrip_with_new_fields() {
        let want = MediaWant::TvSeason {
            show_name: "Test Show".into(),
            season: "2".into(),
            year: Some(2024),
            episodes_needed: None,
            total_episodes: Some(10),
            pack_preference: "auto".into(),
            last_air_date: Some("2024-12-01".into()),
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let json = serde_json::to_string(&want).unwrap();
        let decoded: MediaWant = serde_json::from_str(&json).unwrap();
        match decoded {
            MediaWant::TvSeason {
                total_episodes,
                pack_preference,
                last_air_date,
                ..
            } => {
                assert_eq!(total_episodes, Some(10));
                assert_eq!(pack_preference, "auto");
                assert_eq!(last_air_date, Some("2024-12-01".into()));
            }
            _ => panic!("expected TvSeason"),
        }
    }

    #[test]
    fn test_tv_episode_backward_compat_missing_air_date() {
        let json = r#"{
            "kind": "tv_episode",
            "show_name": "Test",
            "season": "1",
            "episode": "5",
            "year": 2024
        }"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::TvEpisode { air_date, .. } => assert_eq!(air_date, None),
            _ => panic!("expected TvEpisode"),
        }
    }

    #[test]
    fn test_movie_backward_compat_missing_release_date() {
        let json = r#"{
            "kind": "movie",
            "title": "Test Movie",
            "year": 2024
        }"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::Movie { release_date, .. } => assert_eq!(release_date, None),
            _ => panic!("expected Movie"),
        }
    }

    #[test]
    fn test_book_backward_compat_missing_release_date() {
        let json = r#"{
            "kind": "book",
            "title": "Test Book"
        }"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::Book { release_date, .. } => assert_eq!(release_date, None),
            _ => panic!("expected Book"),
        }
    }

    #[test]
    fn test_alternate_titles_backward_compat() {
        // Old payloads without alternate_titles deserialize as None
        let json = r#"{"kind":"movie","title":"Scream 7","year":2026}"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::Movie {
                alternate_titles, ..
            } => assert_eq!(alternate_titles, None),
            _ => panic!("expected Movie"),
        }

        let json = r#"{"kind":"tv_episode","show_name":"Show","season":"1","episode":"5"}"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::TvEpisode {
                alternate_titles, ..
            } => assert_eq!(alternate_titles, None),
            _ => panic!("expected TvEpisode"),
        }

        let json = r#"{"kind":"tv_season","show_name":"Show","season":"1"}"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::TvSeason {
                alternate_titles, ..
            } => assert_eq!(alternate_titles, None),
            _ => panic!("expected TvSeason"),
        }
    }

    #[test]
    fn test_alternate_titles_roundtrip() {
        let want = MediaWant::Movie {
            title: "Scream 7".into(),
            year: Some(2026),
            release_date: None,
            identifiers: None,
            alternate_titles: Some(vec!["Scream VII".into()]),
            quality_profile_id: None,
            quality_overrides: None,
            in_library: None,
            library_video_height: None,
        };
        let json = serde_json::to_string(&want).unwrap();
        assert!(json.contains("alternate_titles"));
        let decoded: MediaWant = serde_json::from_str(&json).unwrap();
        match decoded {
            MediaWant::Movie {
                alternate_titles, ..
            } => assert_eq!(alternate_titles, Some(vec!["Scream VII".into()])),
            _ => panic!("expected Movie"),
        }
    }

    #[test]
    fn test_episode_ref_backward_compat_missing_library_fields() {
        let json = r#"{"season":"1","episode":"5","sort_order":5}"#;
        let ep: super::EpisodeRef = serde_json::from_str(json).unwrap();
        assert_eq!(ep.in_library, None);
        assert_eq!(ep.library_video_height, None);
    }

    #[test]
    fn test_episode_ref_roundtrip_with_library_fields() {
        let ep = super::EpisodeRef {
            season: "1".into(),
            episode: "5".into(),
            sort_order: Some(5),
            air_date: None,
            in_library: Some(true),
            library_video_height: Some(1080),
            title: None,
        };
        let json = serde_json::to_string(&ep).unwrap();
        let decoded: super::EpisodeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.in_library, Some(true));
        assert_eq!(decoded.library_video_height, Some(1080));
    }

    #[test]
    fn test_movie_backward_compat_missing_library_fields() {
        let json = r#"{"kind":"movie","title":"Test","year":2024}"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::Movie {
                in_library,
                library_video_height,
                ..
            } => {
                assert_eq!(in_library, None);
                assert_eq!(library_video_height, None);
            }
            _ => panic!("expected Movie"),
        }
    }

    #[test]
    fn test_sport_event_backward_compat_missing_event_date() {
        let json = r#"{
            "kind": "sport_event",
            "league_name": "F1",
            "season": "2025"
        }"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        match want {
            MediaWant::SportEvent { event_date, .. } => assert_eq!(event_date, None),
            _ => panic!("expected SportEvent"),
        }
    }

    fn sport_event_with_every_alias_field() -> MediaWant {
        MediaWant::SportEvent {
            league_name: "MotoGP".into(),
            season: "2026".into(),
            round: Some(3),
            location: Some("USA".into()),
            session_type: Some("Race".into()),
            event_title: Some("USA - Race".into()),
            event_date: Some("2026-03-29".into()),
            quality_profile_id: None,
            quality_overrides: None,
            in_library: None,
            library_video_height: None,
            meta: Box::new(SportEventMeta {
                venue: Some("Circuit of the Americas".into()),
                city: Some("Austin".into()),
                region: Some("Texas".into()),
                country: Some("United States".into()),
                country_iso: Some("US".into()),
                location_aliases: Some(vec!["COTA".into(), "Austin".into(), "Texas".into()]),
                event_aliases: Some(vec!["Red Bull Grand Prix of The United States".into()]),
                league_aliases: Some(vec!["Moto GP".into()]),
                teams: Some(vec![SportTeam {
                    name: "Ducati Lenovo Team".into(),
                    short: Some("Ducati".into()),
                    aliases: vec!["Ducati Corse".into()],
                }]),
                session_start: Some("2026-03-29T19:00:00Z".into()),
                class: Some("MotoGP".into()),
                identifiers: Some(SportIdentifiers {
                    thesportsdb_id: Some("2424575".into()),
                    sportarr_id: Some("ev-361462".into()),
                    official_id: Some("782c929d-faaf-44e2-9d6f-0fde033855be".into()),
                }),
            }),
        }
    }

    #[test]
    fn sport_event_alias_fields_default_to_none_on_old_payloads() {
        let json = r#"{"kind":"sport_event","league_name":"SBK","season":"2026","round":12,"location":"Spain","session_type":"Race 1"}"#;
        let want: MediaWant = serde_json::from_str(json).unwrap();
        let MediaWant::SportEvent { meta, .. } = want else {
            panic!("expected SportEvent");
        };
        let SportEventMeta {
            venue,
            city,
            region,
            country,
            country_iso,
            location_aliases,
            event_aliases,
            league_aliases,
            teams,
            session_start,
            class,
            identifiers,
        } = *meta;
        assert_eq!(venue, None);
        assert_eq!(city, None);
        assert_eq!(region, None);
        assert_eq!(country, None);
        assert_eq!(country_iso, None);
        assert_eq!(location_aliases, None);
        assert_eq!(event_aliases, None);
        assert_eq!(league_aliases, None);
        assert!(teams.is_none());
        assert_eq!(session_start, None);
        assert_eq!(class, None);
        assert!(identifiers.is_none());
    }

    #[test]
    fn sport_event_roundtrips_every_alias_field() {
        let json = serde_json::to_value(sport_event_with_every_alias_field()).unwrap();
        let decoded: MediaWant = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), json);
        assert_eq!(json["location_aliases"][0], "COTA");
        assert_eq!(json["teams"][0]["short"], "Ducati");
        assert_eq!(
            json["identifiers"]["official_id"],
            "782c929d-faaf-44e2-9d6f-0fde033855be"
        );
        assert_eq!(json["class"], "MotoGP");
    }

    #[test]
    fn sport_event_omits_unset_alias_fields() {
        let want = MediaWant::SportEvent {
            league_name: "SBK".into(),
            season: "2026".into(),
            round: None,
            location: None,
            session_type: None,
            event_title: None,
            event_date: None,
            quality_profile_id: None,
            quality_overrides: None,
            in_library: None,
            library_video_height: None,
            meta: Box::default(),
        };
        let json = serde_json::to_value(want).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for absent in [
            "venue",
            "city",
            "region",
            "country",
            "country_iso",
            "location_aliases",
            "event_aliases",
            "league_aliases",
            "teams",
            "session_start",
            "class",
            "identifiers",
        ] {
            assert!(!keys.contains(&absent), "{absent} serialised while unset");
        }
    }

    /// What a consumer built before these fields existed sees: the variant as
    /// it was, which must still accept a payload carrying all of them.
    #[derive(Debug, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum PreAliasWant {
        SportEvent {
            league_name: String,
            season: String,
            round: Option<u32>,
            location: Option<String>,
            session_type: Option<String>,
        },
        SportSeason {
            league_name: String,
            season: String,
        },
        Book {
            title: String,
        },
        Audiobook {
            title: String,
        },
    }

    #[test]
    fn pre_alias_consumers_accept_alias_payloads() {
        let json = serde_json::to_value(sport_event_with_every_alias_field()).unwrap();
        let PreAliasWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
        } = serde_json::from_value(json).unwrap()
        else {
            panic!("expected SportEvent");
        };
        assert_eq!(league_name, "MotoGP");
        assert_eq!(season, "2026");
        assert_eq!(round, Some(3));
        assert_eq!(location.as_deref(), Some("USA"));
        assert_eq!(session_type.as_deref(), Some("Race"));

        let season = MediaWant::SportSeason {
            league_name: "SBK".into(),
            season: "2026".into(),
            league_aliases: Some(vec!["WSBK".into()]),
            quality_profile_id: None,
            quality_overrides: None,
        };
        let PreAliasWant::SportSeason {
            league_name,
            season,
        } = serde_json::from_value(serde_json::to_value(season).unwrap()).unwrap()
        else {
            panic!("expected SportSeason");
        };
        assert_eq!((league_name.as_str(), season.as_str()), ("SBK", "2026"));

        for kind in ["book", "audiobook"] {
            let json = serde_json::json!({
                "kind": kind,
                "title": "The Fellowship of the Ring",
                "alternate_titles": ["Fellowship of the Ring"],
            });
            let (PreAliasWant::Book { title } | PreAliasWant::Audiobook { title }) =
                serde_json::from_value(json).unwrap()
            else {
                panic!("expected {kind}");
            };
            assert_eq!(title, "The Fellowship of the Ring");
        }
    }

    #[test]
    fn sport_season_league_aliases_roundtrip_and_default() {
        let old: MediaWant =
            serde_json::from_str(r#"{"kind":"sport_season","league_name":"SBK","season":"2026"}"#)
                .unwrap();
        assert!(matches!(
            old,
            MediaWant::SportSeason {
                league_aliases: None,
                ..
            }
        ));
        let want = MediaWant::SportSeason {
            league_name: "SBK".into(),
            season: "2026".into(),
            league_aliases: Some(vec!["WSBK".into(), "World Superbike".into()]),
            quality_profile_id: None,
            quality_overrides: None,
        };
        let decoded: MediaWant =
            serde_json::from_value(serde_json::to_value(want).unwrap()).unwrap();
        let MediaWant::SportSeason { league_aliases, .. } = decoded else {
            panic!("expected SportSeason");
        };
        assert_eq!(
            league_aliases,
            Some(vec!["WSBK".into(), "World Superbike".into()])
        );
    }

    #[test]
    fn book_alternate_titles_roundtrip_and_default() {
        for kind in ["book", "audiobook"] {
            let old: MediaWant =
                serde_json::from_value(serde_json::json!({"kind": kind, "title": "Dune"})).unwrap();
            let json = serde_json::to_value(&old).unwrap();
            assert!(json.get("alternate_titles").is_none(), "{kind}");

            let with = serde_json::json!({
                "kind": kind,
                "title": "Dune",
                "alternate_titles": ["Dune (Dune Chronicles 1)"],
            });
            let decoded: MediaWant = serde_json::from_value(with.clone()).unwrap();
            let titles = match decoded {
                MediaWant::Book {
                    alternate_titles, ..
                }
                | MediaWant::Audiobook {
                    alternate_titles, ..
                } => alternate_titles,
                _ => panic!("expected {kind}"),
            };
            assert_eq!(titles, Some(vec!["Dune (Dune Chronicles 1)".into()]));
        }
    }
}
