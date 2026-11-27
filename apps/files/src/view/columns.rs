//! The details view's columns (P3-files-app 2.2, F1a.3).

use oled_ui::model::{Align, ColId, Column, SortDir};
use oled_ui::UiLang;

use crate::listing::SortKey;
use crate::model::ColKind;
use crate::tr::{k, t};

/// Name flex min 200 default 320, Date modified 150, Type 140, Size 90 End; optional hidden: Created 150, Attributes 90,
/// Status 120. Name is the only column that cannot be hidden.
pub fn default_columns(lang: UiLang) -> Vec<Column> {
    // kind, title, width, min, align, first click, shown
    let rows = [
        (ColKind::Name, k::FILES_COL_NAME, 320, 200, Align::Start, SortDir::Asc, true),
        (ColKind::Modified, k::FILES_COL_MODIFIED, 150, 110, Align::Start, SortDir::Desc, true),
        (ColKind::Type, k::FILES_COL_TYPE, 140, 90, Align::Start, SortDir::Asc, true),
        (ColKind::Size, k::FILES_COL_SIZE, 90, 70, Align::End, SortDir::Desc, true),
        (ColKind::Created, k::FILES_COL_CREATED, 150, 110, Align::Start, SortDir::Desc, false),
        (ColKind::Attributes, k::FILES_COL_ATTRIBUTES, 90, 70, Align::Start, SortDir::Asc, false),
        (ColKind::Status, k::FILES_COL_STATUS, 120, 90, Align::Start, SortDir::Asc, false),
    ];
    rows.into_iter()
        .map(|(kind, title, width, min, align, first_sort, visible)| Column {
            id: ColId(kind as u16),
            title: t(title, lang),
            width,
            min,
            flex: kind == ColKind::Name,
            align,
            sort: None,
            first_sort,
            visible,
            hideable: kind != ColKind::Name,
            tip: None,
        })
        .collect()
}

/// Name, Modified, Type, Size and Created sort; the others do not.
pub fn sort_key(c: ColKind) -> Option<SortKey> {
    match c {
        ColKind::Name => Some(SortKey::Name),
        ColKind::Modified => Some(SortKey::Modified),
        ColKind::Type => Some(SortKey::Type),
        ColKind::Size => Some(SortKey::Size),
        ColKind::Created => Some(SortKey::Created),
        ColKind::Attributes | ColKind::Status => None,
    }
}
