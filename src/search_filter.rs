// Copyright 2024 Mirror OS Contributors
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{Element, widget};

use crate::app_info::AppInfo;
use crate::Message;

/// Which package source to include in search results
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilterSource {
    Flatpak,
    Nix,
}

/// Maximum OARS content rating to include
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentRating {
    /// Only "all-ages" rated content
    All,
    /// "all" + "moderate"
    Moderate,
    /// No restriction
    Intense,
}

/// Accumulated search filter state
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchFilter {
    /// None means "all sources"
    pub source: Option<FilterSource>,
    /// AppStream category string, e.g. "Game", "AudioVideo"
    pub category: Option<String>,
    pub content_rating: Option<ContentRating>,
    pub verified_only: bool,
    pub installed_only: bool,
}

impl SearchFilter {
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.category.is_none()
            && self.content_rating.is_none()
            && !self.verified_only
            && !self.installed_only
    }

    /// Returns `true` if `info` passes all active filter criteria.
    /// `installed` is `true` when the package is currently installed.
    pub fn matches(&self, info: &AppInfo, installed: bool) -> bool {
        // Source filter
        if let Some(source) = &self.source {
            let is_flatpak = info.source_id == "flathub";
            match source {
                FilterSource::Flatpak if !is_flatpak => return false,
                FilterSource::Nix if is_flatpak => return false,
                _ => {}
            }
        }

        // Category filter
        if let Some(cat) = &self.category {
            if !info.categories.iter().any(|c| c == cat) {
                return false;
            }
        }

        // Content rating filter
        if let Some(rating) = &self.content_rating {
            match rating {
                ContentRating::All => {
                    if info.content_rating != "all" {
                        return false;
                    }
                }
                ContentRating::Moderate => {
                    if info.content_rating == "intense" {
                        return false;
                    }
                }
                ContentRating::Intense => {} // no restriction
            }
        }

        // Verified filter
        if self.verified_only && !info.is_verified {
            return false;
        }

        // Installed filter
        if self.installed_only && !installed {
            return false;
        }

        true
    }
}

/// Returns a horizontal chip/pill row that lets the user set filter options.
/// Sends `Message::SearchFilterChanged` on interaction.
pub fn filter_panel<'a>(filter: &'a SearchFilter) -> Element<'a, Message> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let mut row = widget::row::with_capacity(6)
        .spacing(spacing.space_xs)
        .align_y(cosmic::iced::Alignment::Center);

    // Source chips
    {
        let all_active = filter.source.is_none();
        let flatpak_active = filter.source == Some(FilterSource::Flatpak);
        let nix_active = filter.source == Some(FilterSource::Nix);

        let all_btn: Element<'_, Message> = if all_active {
            widget::button::suggested("All").into()
        } else {
            widget::button::standard("All")
                .on_press(Message::SearchFilterChanged(SearchFilter {
                    source: None,
                    ..filter.clone()
                }))
                .into()
        };

        let flatpak_btn: Element<'_, Message> = if flatpak_active {
            widget::button::suggested("Flatpak").into()
        } else {
            widget::button::standard("Flatpak")
                .on_press(Message::SearchFilterChanged(SearchFilter {
                    source: Some(FilterSource::Flatpak),
                    ..filter.clone()
                }))
                .into()
        };

        let nix_btn: Element<'_, Message> = if nix_active {
            widget::button::suggested("Nix").into()
        } else {
            widget::button::standard("Nix")
                .on_press(Message::SearchFilterChanged(SearchFilter {
                    source: Some(FilterSource::Nix),
                    ..filter.clone()
                }))
                .into()
        };

        row = row.push(all_btn).push(flatpak_btn).push(nix_btn);
    }

    // Separator
    row = row.push(
        widget::divider::vertical::default().height(cosmic::iced::Length::Fixed(24.0)),
    );

    // Verified toggle
    {
        let label = if filter.verified_only {
            "Verified ✓"
        } else {
            "Verified"
        };
        let btn: Element<'_, Message> = if filter.verified_only {
            widget::button::suggested(label).into()
        } else {
            widget::button::standard(label)
                .on_press(Message::SearchFilterChanged(SearchFilter {
                    verified_only: true,
                    ..filter.clone()
                }))
                .into()
        };
        row = row.push(btn);
        if filter.verified_only {
            // allow turning it off
            row = row.push(
                widget::button::destructive("×").on_press(Message::SearchFilterChanged(
                    SearchFilter {
                        verified_only: false,
                        ..filter.clone()
                    },
                )),
            );
        }
    }

    widget::container(row)
        .padding([spacing.space_xxs, spacing.space_s])
        .into()
}
