//! The calendar panel's model: month arithmetic, the 7 x 6 grid, names in en / it and the
//! panel rows. Pure logic; the window and the grid drawing live in the app.

use crate::i18n::{tr, Key, Lang};
use crate::panel_kit::{Button, Part, Row};

/// A day of the week; the index counts from Monday (the order of `LOCALE_IFIRSTDAYOFWEEK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

const WEEKDAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

impl Weekday {
    /// 0 = Monday ... 6 = Sunday; anything above wraps.
    pub fn from_index(i: u32) -> Self {
        WEEKDAYS[(i % 7) as usize]
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Month {
    pub year: i32,
    /// 1..=12.
    pub month: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub fn month(self) -> Month {
        Month {
            year: self.year,
            month: self.month,
        }
    }
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

impl Month {
    pub fn next(self) -> Self {
        if self.month >= 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month.max(1) + 1,
            }
        }
    }

    pub fn prev(self) -> Self {
        if self.month <= 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month.min(12) - 1,
            }
        }
    }

    pub fn days(self) -> u8 {
        match self.month {
            2 if leap(self.year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    pub fn first_weekday(self) -> Weekday {
        weekday_of(Date {
            year: self.year,
            month: self.month,
            day: 1,
        })
    }
}

/// The weekday of `d` (Sakamoto's method).
pub fn weekday_of(d: Date) -> Weekday {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = i32::from(d.month.clamp(1, 12));
    let y = if m < 3 { d.year - 1 } else { d.year };
    // 0 = Sunday.
    let sun0 =
        (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + i32::from(d.day)).rem_euclid(7);
    Weekday::from_index(((sun0 + 6) % 7) as u32)
}

/// The days of `m` in a 7 x 6 grid whose columns start at `first`; `None` outside the month.
pub fn grid(m: Month, first: Weekday) -> [[Option<u8>; 7]; 6] {
    let lead = (m.first_weekday().index() + 7 - first.index()) % 7;
    let mut g = [[None; 7]; 6];
    for d in 1..=m.days() {
        let i = lead + usize::from(d) - 1;
        g[i / 7][i % 7] = Some(d);
    }
    g
}

/// One cell of the shown month, with the neighbouring months' days filling the gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub date: Date,
    pub in_month: bool,
}

pub fn cells(m: Month, first: Weekday) -> [[Cell; 7]; 6] {
    let g = grid(m, first);
    let (p, n) = (m.prev(), m.next());
    let lead = g[0].iter().take_while(|c| c.is_none()).count();
    let blank = Cell {
        date: Date {
            year: m.year,
            month: m.month,
            day: 1,
        },
        in_month: true,
    };
    let mut out = [[blank; 7]; 6];
    let mut after = 0u8;
    for (r, row) in g.iter().enumerate() {
        for (c, day) in row.iter().enumerate() {
            out[r][c] = match day {
                Some(d) => Cell {
                    date: Date {
                        year: m.year,
                        month: m.month,
                        day: *d,
                    },
                    in_month: true,
                },
                None if r == 0 => Cell {
                    date: Date {
                        year: p.year,
                        month: p.month,
                        day: p.days() - (lead - c) as u8 + 1,
                    },
                    in_month: false,
                },
                None => {
                    after += 1;
                    Cell {
                        date: Date {
                            year: n.year,
                            month: n.month,
                            day: after,
                        },
                        in_month: false,
                    }
                }
            };
        }
    }
    out
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_IT: [&str; 12] = [
    "gennaio",
    "febbraio",
    "marzo",
    "aprile",
    "maggio",
    "giugno",
    "luglio",
    "agosto",
    "settembre",
    "ottobre",
    "novembre",
    "dicembre",
];
const DAYS_EN: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const DAYS_IT: [&str; 7] = [
    "lunedì",
    "martedì",
    "mercoledì",
    "giovedì",
    "venerdì",
    "sabato",
    "domenica",
];
const INITIALS_EN: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
const INITIALS_IT: [&str; 7] = ["L", "M", "M", "G", "V", "S", "D"];

fn month_name(month: u8, lang: Lang) -> &'static str {
    let i = usize::from(month.clamp(1, 12)) - 1;
    match lang {
        Lang::En => MONTHS_EN[i],
        Lang::It => MONTHS_IT[i],
    }
}

/// "ottobre 2026" / "October 2026".
pub fn month_title(m: Month, lang: Lang) -> String {
    format!("{} {}", month_name(m.month, lang), m.year)
}

/// "sabato 31 ottobre" / "Saturday, October 31".
pub fn long_date(d: Date, lang: Lang) -> String {
    let w = weekday_of(d).index();
    match lang {
        Lang::En => format!("{}, {} {}", DAYS_EN[w], month_name(d.month, lang), d.day),
        Lang::It => format!("{} {} {}", DAYS_IT[w], d.day, month_name(d.month, lang)),
    }
}

/// The column headings, starting at `first`.
pub fn weekday_initials(first: Weekday, lang: Lang) -> [String; 7] {
    let names = match lang {
        Lang::En => &INITIALS_EN,
        Lang::It => &INITIALS_IT,
    };
    std::array::from_fn(|i| names[(first.index() + i) % 7].to_string())
}

/// Milliseconds from the given local time of day to the next midnight.
pub fn ms_until_midnight(hour: u32, minute: u32, second: u32, ms: u32) -> u64 {
    let elapsed = ((u64::from(hour) * 60 + u64::from(minute)) * 60 + u64::from(second)) * 1000
        + u64::from(ms);
    86_400_000u64.saturating_sub(elapsed).max(1)
}

// ---- the panel ----------------------------------------------------------------------------

pub const ID_PREV: u32 = 1;
pub const ID_NEXT: u32 = 2;
pub const ID_GRID: u32 = 3;
pub const ID_NOTIFICATIONS: u32 = 4;
pub const ID_DATE_SETTINGS: u32 = 5;

/// Height of the weekday heading and of each week in the grid hole.
pub const WEEKDAY_H_DIP: f32 = 24.0;
pub const CELL_H_DIP: f32 = 40.0;
pub const GRID_H_DIP: f32 = WEEKDAY_H_DIP + 6.0 * CELL_H_DIP;

/// Segoe Fluent Icons "ChevronUp" / "ChevronDown".
const G_PREV: char = '\u{E70E}';
const G_NEXT: char = '\u{E70D}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PrevMonth,
    NextMonth,
    Notifications,
    DateSettings,
}

pub fn action(p: Part) -> Option<Action> {
    match p {
        Part::HeaderAction(ID_PREV) => Some(Action::PrevMonth),
        Part::HeaderAction(ID_NEXT) => Some(Action::NextMonth),
        Part::Button(ID_NOTIFICATIONS) => Some(Action::Notifications),
        Part::Button(ID_DATE_SETTINGS) => Some(Action::DateSettings),
        _ => None,
    }
}

/// Today's long date, the shown month with its chevrons, the grid hole and the two buttons.
pub fn rows(view: Month, today: Date, lang: Lang) -> Vec<Row> {
    let button = |id, key| {
        Row::Buttons(vec![Button {
            id,
            glyph: None,
            text: tr(lang, key).to_string(),
        }])
    };
    vec![
        Row::Header {
            text: long_date(today, lang),
            trailing: None,
            actions: Vec::new(),
        },
        Row::Header {
            text: month_title(view, lang),
            trailing: None,
            actions: vec![(ID_PREV, G_PREV), (ID_NEXT, G_NEXT)],
        },
        Row::Custom {
            id: ID_GRID,
            h: GRID_H_DIP,
        },
        button(ID_NOTIFICATIONS, Key::Notifications),
        button(ID_DATE_SETTINGS, Key::DateTimeSettings),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel_kit::{layout, MAX_H_DIP};

    const OCT_2026: Month = Month {
        year: 2026,
        month: 10,
    };

    fn m(year: i32, month: u8) -> Month {
        Month { year, month }
    }

    fn d(year: i32, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn october_2026_monday_first() {
        let g = grid(OCT_2026, Weekday::Mon);
        assert_eq!(
            g[0],
            [None, None, None, Some(1), Some(2), Some(3), Some(4)]
        );
        // The 31st is a Saturday in the fifth week.
        assert_eq!(g[4][5], Some(31));
        assert_eq!(g[4][6], None);
        assert!(g[5].iter().all(Option::is_none));
    }

    #[test]
    fn sunday_first_shifts_by_one() {
        let g = grid(OCT_2026, Weekday::Sun);
        assert_eq!(g[0], [None, None, None, None, Some(1), Some(2), Some(3)]);
        assert_eq!(g[4][6], Some(31));
    }

    #[test]
    fn leap_february_has_29_days() {
        assert_eq!(m(2028, 2).days(), 29);
        let n = grid(m(2028, 2), Weekday::Mon).iter().flatten().flatten().count();
        assert_eq!(n, 29);
        assert_eq!(m(2027, 2).days(), 28);
        assert_eq!(m(1900, 2).days(), 28);
        assert_eq!(m(2000, 2).days(), 29);
    }

    #[test]
    fn a_month_that_fills_six_weeks() {
        // August 2026 starts on a Saturday: Monday-first it needs six rows.
        let g = grid(m(2026, 8), Weekday::Mon);
        assert_eq!(g[0][5], Some(1));
        assert_eq!(g[5][0], Some(31));
    }

    #[test]
    fn months_step_over_the_year() {
        assert_eq!(m(2026, 12).next(), m(2027, 1));
        assert_eq!(m(2026, 1).prev(), m(2025, 12));
        assert_eq!(OCT_2026.next().prev(), OCT_2026);
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday_of(d(2026, 10, 31)), Weekday::Sat);
        assert_eq!(weekday_of(d(2000, 1, 1)), Weekday::Sat);
        assert_eq!(weekday_of(d(2024, 2, 29)), Weekday::Thu);
        assert_eq!(weekday_of(d(2026, 1, 1)), Weekday::Thu);
    }

    #[test]
    fn cells_fill_the_gaps_with_the_neighbours() {
        let c = cells(OCT_2026, Weekday::Mon);
        // Monday 28 September ... Wednesday 30 September, then the 1st.
        assert_eq!(
            c[0][0],
            Cell {
                date: d(2026, 9, 28),
                in_month: false
            }
        );
        assert_eq!(c[0][2].date.day, 30);
        assert!(c[0][3].in_month && c[0][3].date.day == 1);
        // After the 31st: November 1st, and the sixth week is November 2nd to 8th.
        assert_eq!(
            c[4][6],
            Cell {
                date: d(2026, 11, 1),
                in_month: false
            }
        );
        assert_eq!(c[5][6].date.day, 8);
        // A month starting on the first column has no leading days.
        let f = cells(m(2026, 6), Weekday::Mon);
        assert!(f[0][0].in_month && f[0][0].date.day == 1);
        // January's lead comes from the previous December.
        let j = cells(m(2026, 1), Weekday::Mon);
        assert_eq!(j[0][0].date, d(2025, 12, 29));
    }

    #[test]
    fn titles_and_dates() {
        assert_eq!(month_title(OCT_2026, Lang::It), "ottobre 2026");
        assert_eq!(month_title(OCT_2026, Lang::En), "October 2026");
        assert_eq!(long_date(d(2026, 10, 31), Lang::It), "sabato 31 ottobre");
        assert_eq!(long_date(d(2026, 10, 31), Lang::En), "Saturday, October 31");
    }

    #[test]
    fn initials_start_at_the_first_weekday() {
        assert_eq!(weekday_initials(Weekday::Mon, Lang::It).join(""), "LMMGVSD");
        assert_eq!(weekday_initials(Weekday::Sun, Lang::It).join(""), "DLMMGVS");
        assert_eq!(weekday_initials(Weekday::Mon, Lang::En).join(""), "MTWTFSS");
        assert_eq!(weekday_initials(Weekday::Sun, Lang::En).join(""), "SMTWTFS");
    }

    #[test]
    fn midnight_countdown() {
        assert_eq!(ms_until_midnight(0, 0, 0, 0), 86_400_000);
        assert_eq!(ms_until_midnight(23, 59, 59, 500), 500);
        assert_eq!(ms_until_midnight(12, 0, 0, 0), 43_200_000);
    }

    #[test]
    fn the_panel_rows() {
        let r = rows(OCT_2026, d(2026, 10, 31), Lang::It);
        assert!(matches!(&r[0], Row::Header { text, .. } if text == "sabato 31 ottobre"));
        assert!(matches!(&r[1], Row::Header { text, actions, .. }
            if text == "ottobre 2026" && actions.len() == 2));
        assert!(r.iter().any(|x| matches!(x, Row::Custom { id: ID_GRID, .. })));
        let l = layout(&r);
        assert_eq!(action(Part::HeaderAction(ID_NEXT)), Some(Action::NextMonth));
        assert_eq!(
            action(Part::Button(ID_NOTIFICATIONS)),
            Some(Action::Notifications)
        );
        assert_eq!(action(Part::Custom(ID_GRID)), None);
        assert!(l
            .parts
            .iter()
            .any(|(p, _)| *p == Part::Button(ID_DATE_SETTINGS)));
        assert!(l.size.1 <= MAX_H_DIP);
    }
}
