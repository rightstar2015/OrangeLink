//! "Group by": the grid split into labelled runs (date added, kind, size).
//!
//! Grouping only reorders the visible items so each group is contiguous and
//! records where each run starts. Everything addressed by index into the visible
//! list (selection, arrow keys, the selection box, opening) keeps working
//! unchanged; only the views insert a header before each run.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/explorer/grouping.rs`),
//! MIT License, Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use crate::model::{Item, MediaKind};
use chrono::{Datelike, Days, NaiveDate};
use serde::{Deserialize, Serialize};

/// What the grid is grouped by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupBy {
    /// One flat grid.
    #[default]
    None,
    /// Today, yesterday, this week… newest first.
    Added,
    /// Images, videos, audio, other.
    Kind,
    /// Empty, tiny, small… smallest first.
    Size,
}

impl GroupBy {
    pub const ALL: [GroupBy; 4] = [GroupBy::None, GroupBy::Added, GroupBy::Kind, GroupBy::Size];

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::None => "不分組",
            GroupBy::Added => "加入日期",
            GroupBy::Kind => "類型",
            GroupBy::Size => "檔案大小",
        }
    }
}

/// One labelled run of the visible items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub label: std::borrow::Cow<'static, str>,
    /// Index of the run's first item.
    pub start: usize,
    pub len: usize,
}

impl Group {
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.start + self.len
    }
}

/// Reorders `items` so each group is one run, keeping the current sort inside
/// every group, and returns the runs in display order. `GroupBy::None` leaves the
/// items alone and returns no groups. `today` is the local date.
pub fn group_items(items: &mut [&Item], by: GroupBy, today: NaiveDate) -> Vec<Group> {
    if by == GroupBy::None {
        return Vec::new();
    }
    // A stable sort by the group's rank: within a group the order the items were
    // already sorted in survives.
    items.sort_by_cached_key(|it| group_of(it, by, today).0);
    let mut groups: Vec<Group> = Vec::new();
    for (index, it) in items.iter().enumerate() {
        let (_, label) = group_of(it, by, today);
        match groups.last_mut() {
            Some(group) if group.label == label => group.len += 1,
            _ => groups.push(Group { label: label.into(), start: index, len: 1 }),
        }
    }
    groups
}

/// The group an item falls in, as (display rank, label).
fn group_of(it: &Item, by: GroupBy, today: NaiveDate) -> (u8, &'static str) {
    match by {
        GroupBy::None => (0, ""),
        GroupBy::Kind => {
            let rank = MediaKind::ALL.iter().position(|k| *k == it.kind).unwrap_or(MediaKind::ALL.len());
            (rank as u8, it.kind.label())
        }
        GroupBy::Added => {
            let Some(date) = chrono::DateTime::from_timestamp_millis(it.added_at).map(|d| d.with_timezone(&chrono::Local).date_naive()) else {
                return (7, "沒有日期");
            };
            let week_start = today - Days::new(u64::from(today.weekday().num_days_from_monday()));
            if date >= today {
                (0, "今天")
            } else if Some(date) == today.pred_opt() {
                (1, "昨天")
            } else if date >= week_start {
                (2, "本週")
            } else if date.year() == today.year() && date.month() == today.month() {
                (3, "本月")
            } else if date.year() == today.year() {
                (4, "今年")
            } else if date.year() == today.year() - 1 {
                (5, "去年")
            } else {
                (6, "更早")
            }
        }
        // Explorer's own size bands.
        GroupBy::Size => match it.size {
            0 => (0, "空的"),
            s if s < 16 * 1024 => (1, "極小（16 KB 以下）"),
            s if s < 1024 * 1024 => (2, "小（16 KB 到 1 MB）"),
            s if s < 128 * 1024 * 1024 => (3, "中（1 到 128 MB）"),
            s if s < 1024 * 1024 * 1024 => (4, "大（128 MB 到 1 GB）"),
            s if s < 4 * 1024 * 1024 * 1024 => (5, "很大（1 到 4 GB）"),
            _ => (6, "超大（4 GB 以上）"),
        },
    }
}

/// One row of the grid or the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The header of `groups[index]`.
    Header(usize),
    /// Items `first..end` of the visible list.
    Items { first: usize, end: usize },
}

/// The rows: `per_row` items to a row, each group starting on a row of its own
/// under its header. The list is the case `per_row == 1`.
pub fn rows(groups: &[Group], items: usize, per_row: usize) -> Vec<Row> {
    let per_row = per_row.max(1);
    let runs = |start: usize, len: usize| (start..start + len).step_by(per_row).map(move |first| Row::Items { first, end: (first + per_row).min(start + len) });
    if groups.is_empty() {
        return runs(0, items).collect();
    }
    let mut out = Vec::with_capacity(items / per_row + groups.len() * 2);
    for (index, group) in groups.iter().enumerate() {
        out.push(Row::Header(index));
        out.extend(runs(group.start, group.len));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, kind: &str, size: u64, added: NaiveDate) -> Item {
        let added_at = added.and_hms_opt(12, 0, 0).unwrap().and_local_timezone(chrono::Local).unwrap().timestamp_millis();
        serde_json::from_value(serde_json::json!({
            "id": name, "name": name, "file_name": name, "ext": "x", "kind": kind, "size": size,
            "width": 0, "height": 0, "added_at": added_at, "modified_at": 0
        }))
        .unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn names<'a>(items: &[&'a Item]) -> Vec<&'a str> {
        items.iter().map(|it| it.name.as_str()).collect()
    }

    fn labels(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|g| &*g.label).collect()
    }

    #[test]
    fn kind_groups_are_contiguous_and_keep_the_sort_inside() {
        let day = date(2026, 11, 3);
        let all = [item("b.png", "image", 1, day), item("a.mp4", "video", 1, day), item("a.png", "image", 1, day), item("z", "other", 1, day)];
        let mut items: Vec<&Item> = all.iter().collect();
        let groups = group_items(&mut items, GroupBy::Kind, day);
        assert_eq!(names(&items), ["b.png", "a.png", "a.mp4", "z"]);
        assert_eq!(labels(&groups), ["圖片", "影片", "其他"]);
        assert_eq!(groups[0], Group { label: "圖片".into(), start: 0, len: 2 });
    }

    #[test]
    fn date_groups_follow_the_calendar() {
        // Thursday 5 November 2026; the week began on Monday the 2nd.
        let today = date(2026, 11, 5);
        let all = [
            item("old", "image", 1, date(2020, 1, 1)),
            item("today", "image", 1, today),
            item("monday", "image", 1, date(2026, 11, 2)),
            item("sunday", "image", 1, date(2026, 11, 1)),
            item("yesterday", "image", 1, date(2026, 11, 4)),
            item("october", "image", 1, date(2026, 10, 1)),
            item("lastyear", "image", 1, date(2025, 6, 1)),
        ];
        let mut items: Vec<&Item> = all.iter().collect();
        let groups = group_items(&mut items, GroupBy::Added, today);
        assert_eq!(labels(&groups), ["今天", "昨天", "本週", "本月", "今年", "去年", "更早"]);
        assert_eq!(names(&items)[..3], ["today", "yesterday", "monday"]);
        // Sunday the 1st is last week but this month.
        assert_eq!(items[3].name, "sunday");
    }

    #[test]
    fn size_groups_grow() {
        let day = date(2026, 11, 3);
        let all = [item("big", "video", 2 * 1024 * 1024 * 1024, day), item("empty", "other", 0, day), item("tiny", "image", 10, day)];
        let mut items: Vec<&Item> = all.iter().collect();
        let groups = group_items(&mut items, GroupBy::Size, day);
        assert_eq!(names(&items), ["empty", "tiny", "big"]);
        assert_eq!(groups.len(), 3);
        assert!(group_items(&mut items, GroupBy::None, day).is_empty());
    }

    #[test]
    fn rows_put_a_header_before_each_group() {
        let groups = vec![Group { label: "A".into(), start: 0, len: 3 }, Group { label: "B".into(), start: 3, len: 1 }];
        assert_eq!(
            rows(&groups, 4, 2),
            [Row::Header(0), Row::Items { first: 0, end: 2 }, Row::Items { first: 2, end: 3 }, Row::Header(1), Row::Items { first: 3, end: 4 }]
        );
        assert_eq!(rows(&groups, 4, 1).len(), 6);
        assert_eq!(rows(&[], 3, 2), [Row::Items { first: 0, end: 2 }, Row::Items { first: 2, end: 3 }]);
    }
}
