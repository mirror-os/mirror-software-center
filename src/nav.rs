// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use cosmic::widget;

use crate::app_id::AppId;
use crate::fl;
use crate::icon_cache::icon_cache_icon;

// From https://specifications.freedesktop.org/menu-spec/latest/apa.html
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Category {
    AudioVideo,
    Development,
    Education,
    Game,
    Graphics,
    Network,
    Office,
    Science,
    Settings,
    System,
    Utility,
    CosmicApplet,
}

impl Category {
    pub fn id(&self) -> &'static str {
        match self {
            Self::AudioVideo => "AudioVideo",
            Self::Development => "Development",
            Self::Education => "Education",
            Self::Game => "Game",
            Self::Graphics => "Graphics",
            Self::Network => "Network",
            Self::Office => "Office",
            Self::Science => "Science",
            Self::Settings => "Settings",
            Self::System => "System",
            Self::Utility => "Utility",
            Self::CosmicApplet => "CosmicApplet",
        }
    }
}

pub type CategoryIndex = HashMap<String, Vec<AppId>>;

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub enum NavPage {
    #[default]
    Explore,
    Applets,
    Installed,
    Updates,
}

impl NavPage {
    pub fn all() -> &'static [Self] {
        &[
            Self::Explore,
            Self::Applets,
            Self::Installed,
            Self::Updates,
        ]
    }

    pub fn title(&self) -> String {
        match self {
            Self::Explore => fl!("explore"),
            Self::Applets => fl!("applets"),
            Self::Installed => fl!("installed-apps"),
            Self::Updates => fl!("updates"),
        }
    }

    // From https://specifications.freedesktop.org/menu-spec/latest/apa.html
    pub fn categories(&self) -> Option<&'static [Category]> {
        match self {
            Self::Applets => Some(&[Category::CosmicApplet]),
            _ => None,
        }
    }

    pub fn icon(&self) -> widget::icon::Icon {
        match self {
            Self::Explore => icon_cache_icon("store-home-symbolic", 16),
            Self::Applets => icon_cache_icon("store-applets-symbolic", 16),
            Self::Installed => icon_cache_icon("store-installed-symbolic", 16),
            Self::Updates => icon_cache_icon("store-updates-symbolic", 16),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScrollContext {
    NavPage,
    ExplorePage,
    SearchResults,
    Selected,
}

impl ScrollContext {
    pub fn unused_contexts(&self) -> &'static [ScrollContext] {
        // Contexts that can be safely removed when another is active
        match self {
            Self::NavPage => &[Self::Selected, Self::SearchResults, Self::ExplorePage],
            Self::ExplorePage => &[Self::Selected, Self::SearchResults],
            Self::SearchResults => &[Self::Selected],
            Self::Selected => &[],
        }
    }
}
