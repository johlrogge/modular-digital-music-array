use chrono::Datelike;
use chrono::NaiveDate;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum DateExpressionError {
    #[error("invalid date expression '{0}'")]
    Invalid(String),
    #[error("too many components in date expression '{0}': expected at most 3 (year/month/day)")]
    TooManyComponents(String),
    #[error("invalid component '{component}' in date expression '{expr}'")]
    InvalidComponent { component: String, expr: String },
    #[error("date out of range in expression '{0}'")]
    OutOfRange(String),
}

/// Resolve a date expression relative to `today`.
///
/// Syntax: components separated by `/` — year, month, day (left to right).
/// - `~` = current value for that position
/// - `~+N` / `~-N` = current value ± N
/// - `+N` / `-N` = current value ± N (shorthand)
/// - `^` = first (1 for month/day; error for year)
/// - `$` = last (12 for month, last day of month for day; error for year)
/// - plain integer = absolute value
///
/// Component count determines interpretation (least-significant first):
/// - 1 part:  day   (year/month from today)
/// - 2 parts: month/day  (year from today)
/// - 3 parts: year/month/day
///
/// Examples:
/// - `"~"` → today
/// - `"-7"` → 7 days ago
/// - `"^"` → 1st of current month
/// - `"$"` → last day of current month
/// - `"-3/2"` → 3 months ago, day 2
/// - `"~/+1/15"` → 15th of next month
/// - `"+1/2/1"` → February 1st of next year
pub fn resolve(expr: &str, today: NaiveDate) -> Result<NaiveDate, DateExpressionError> {
    let parts: Vec<&str> = expr.split('/').collect();

    if parts.len() > 3 {
        return Err(DateExpressionError::TooManyComponents(expr.to_string()));
    }

    let cur_year = today.year();
    let cur_month = today.month() as i32;
    let cur_day = today.day() as i32;

    // Single component uses chrono::Duration for day arithmetic to correctly
    // cross month/year boundaries, unlike the multi-component path which uses
    // simple integer arithmetic with clamping.
    // Fast path: single component → day offset/absolute with correct month/year boundary crossing
    if parts.len() == 1 {
        let s = parts[0].trim();
        if s == "~" {
            return Ok(today);
        }
        if s == "^" {
            return NaiveDate::from_ymd_opt(cur_year, cur_month as u32, 1)
                .ok_or_else(|| DateExpressionError::OutOfRange(expr.to_string()));
        }
        if s == "$" {
            let last = days_in_month(cur_year, cur_month as u32);
            return NaiveDate::from_ymd_opt(cur_year, cur_month as u32, last as u32)
                .ok_or_else(|| DateExpressionError::OutOfRange(expr.to_string()));
        }
        // Relative offset: +N, -N, ~+N, ~-N
        let delta_str = s.strip_prefix('~').unwrap_or(s);
        if delta_str.starts_with('+')
            || (delta_str.starts_with('-')
                && delta_str.len() > 1
                && delta_str[1..].chars().all(|c| c.is_ascii_digit()))
        {
            let delta: i64 =
                delta_str
                    .parse()
                    .map_err(|_| DateExpressionError::InvalidComponent {
                        component: s.to_string(),
                        expr: expr.to_string(),
                    })?;
            return today
                .checked_add_signed(chrono::Duration::days(delta))
                .ok_or_else(|| DateExpressionError::OutOfRange(expr.to_string()));
        }
        // Absolute day number
        if let Ok(day) = s.parse::<u32>() {
            return NaiveDate::from_ymd_opt(cur_year, cur_month as u32, day)
                .ok_or_else(|| DateExpressionError::OutOfRange(expr.to_string()));
        }
        return Err(DateExpressionError::InvalidComponent {
            component: s.to_string(),
            expr: expr.to_string(),
        });
    }

    // Interpretation depends on number of components:
    // 2 parts: month/day  (year from today)
    // 3 parts: year/month/day
    let (year_str, month_str, day_str): (Option<&str>, Option<&str>, Option<&str>) =
        match parts.as_slice() {
            [d] => (None, None, Some(d)),
            [m, d] => (None, Some(m), Some(d)),
            [y, m, d] => (Some(y), Some(m), Some(d)),
            _ => unreachable!(),
        };

    let year = if let Some(s) = year_str {
        resolve_component(s, cur_year, None, None, expr)?
    } else {
        cur_year
    };

    let month_raw = if let Some(s) = month_str {
        resolve_component(s, cur_month, Some(1), Some(12), expr)?
    } else {
        cur_month
    };

    // Wrap month overflow/underflow into year adjustments
    let (adj_year, final_month) = normalize_month(year, month_raw);

    // Parse day component
    let last_day_of_month = days_in_month(adj_year, final_month as u32);

    let day_raw = if let Some(s) = day_str {
        resolve_component(s, cur_day, Some(1), Some(last_day_of_month), expr)?
    } else {
        cur_day
    };

    // Clamp day to month length
    let final_day = day_raw.max(1).min(last_day_of_month) as u32;

    NaiveDate::from_ymd_opt(adj_year, final_month as u32, final_day)
        .ok_or_else(|| DateExpressionError::OutOfRange(expr.to_string()))
}

/// Resolve a single component string given the current value and optional first/last bounds.
fn resolve_component(
    s: &str,
    current: i32,
    first: Option<i32>,
    last: Option<i32>,
    expr: &str,
) -> Result<i32, DateExpressionError> {
    let err = || DateExpressionError::InvalidComponent {
        component: s.to_string(),
        expr: expr.to_string(),
    };

    let s = s.trim();

    if s == "^" {
        return first.ok_or_else(err);
    }

    if s == "$" {
        return last.ok_or_else(err);
    }

    // `~` alone
    if s == "~" {
        return Ok(current);
    }

    // `~+N` or `~-N`
    if let Some(rest) = s.strip_prefix('~') {
        if rest.is_empty() {
            return Ok(current);
        }
        let delta: i32 = rest.parse().map_err(|_| err())?;
        return Ok(current + delta);
    }

    // `+N` or `-N` shorthand (offset from current)
    if s.starts_with('+')
        || (s.starts_with('-') && s.len() > 1 && s[1..].chars().all(|c| c.is_ascii_digit()))
    {
        let delta: i32 = s.parse().map_err(|_| err())?;
        return Ok(current + delta);
    }

    // Plain integer
    let value: i32 = s.parse().map_err(|_| err())?;
    Ok(value)
}

/// Normalize a (year, month) pair so month is in 1..=12, adjusting year.
fn normalize_month(year: i32, month: i32) -> (i32, i32) {
    // month may be 0 or negative or > 12
    let month_zero_indexed = month - 1; // 0-indexed
    let year_offset = month_zero_indexed.div_euclid(12);
    let final_month = month_zero_indexed.rem_euclid(12) + 1;
    (year + year_offset, final_month)
}

/// Returns the number of days in a given year/month.
fn days_in_month(year: i32, month: u32) -> i32 {
    // First day of next month minus one day
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    let first_of_next = NaiveDate::from_ymd_opt(next_year, next_month, 1).unwrap();
    let last = first_of_next.pred_opt().unwrap();
    last.day() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // --- today ---

    #[test]
    fn tilde_alone_is_today() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~", today).unwrap(), today);
    }

    // --- single component (day only, year/month from today) ---

    #[test]
    fn single_day_absolute() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("15", today).unwrap(), date(2026, 3, 15));
    }

    #[test]
    fn single_day_offset_minus_1() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~-1", today).unwrap(), date(2026, 3, 7));
    }

    #[test]
    fn single_day_offset_plus_2() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~+2", today).unwrap(), date(2026, 3, 10));
    }

    #[test]
    fn single_day_shorthand_minus() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("-1", today).unwrap(), date(2026, 3, 7));
    }

    #[test]
    fn single_caret_is_first_of_month() {
        let today = date(2026, 3, 15);
        assert_eq!(resolve("^", today).unwrap(), date(2026, 3, 1));
    }

    #[test]
    fn single_dollar_is_last_of_month() {
        let today = date(2026, 3, 15);
        assert_eq!(resolve("$", today).unwrap(), date(2026, 3, 31));
    }

    #[test]
    fn caret_in_year_position_is_error() {
        let today = date(2026, 3, 8);
        assert!(resolve("^/3/15", today).is_err());
    }

    #[test]
    fn dollar_in_year_position_is_error() {
        let today = date(2026, 3, 8);
        assert!(resolve("$/3/15", today).is_err());
    }

    // --- two components (month/day, implicit current year) ---

    #[test]
    fn two_components_minus_3_months_day_2() {
        // "-3/2" = 3 months ago (from current month), day 2 (of that month)
        let today = date(2026, 3, 8);
        // current month is 3, -3 = month 0 → december 2025
        assert_eq!(resolve("-3/2", today).unwrap(), date(2025, 12, 2));
    }

    #[test]
    fn two_components_tilde_slash_tilde_is_today() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/~", today).unwrap(), date(2026, 3, 8));
    }

    #[test]
    fn two_components_month_overflow_wraps_year() {
        // month +11 from march = february next year, day = today's day (8)
        let today = date(2026, 3, 8);
        assert_eq!(resolve("+11/~", today).unwrap(), date(2027, 2, 8));
    }

    #[test]
    fn two_components_month_underflow_wraps_year_back() {
        // march - 3 = december previous year, day = today's day (8)
        let today = date(2026, 3, 8);
        assert_eq!(resolve("-3/~", today).unwrap(), date(2025, 12, 8));
    }

    // --- three components ---

    #[test]
    fn three_components_first_day_of_current_month() {
        let today = date(2026, 3, 15);
        assert_eq!(resolve("~/~/^", today).unwrap(), date(2026, 3, 1));
    }

    #[test]
    fn three_components_last_day_of_current_month() {
        let today = date(2026, 3, 15);
        assert_eq!(resolve("~/~/$", today).unwrap(), date(2026, 3, 31));
    }

    #[test]
    fn three_components_last_day_of_february_non_leap() {
        let today = date(2026, 3, 15);
        assert_eq!(resolve("~/2/$", today).unwrap(), date(2026, 2, 28));
    }

    #[test]
    fn three_components_last_day_of_february_leap_year() {
        let today = date(2024, 3, 15);
        assert_eq!(resolve("~/2/$", today).unwrap(), date(2024, 2, 29));
    }

    #[test]
    fn three_components_absolute() {
        let today = date(2026, 3, 8);
        assert_eq!(resolve("2024/6/15", today).unwrap(), date(2024, 6, 15));
    }

    // --- day clamping ---

    #[test]
    fn day_clamped_to_month_length() {
        // January has 31 days, February has 28 in 2026
        // If today is jan 31 and we move to february keeping the day:
        let today = date(2026, 1, 31);
        assert_eq!(resolve("~/2/~", today).unwrap(), date(2026, 2, 28));
    }

    // --- edge cases ---

    #[test]
    fn current_year_3_months_ago_day_2() {
        // `~/-3/2`
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/-3/2", today).unwrap(), date(2025, 12, 2));
    }

    #[test]
    fn too_many_components_errors() {
        let today = date(2026, 3, 8);
        assert!(resolve("~/~/~/~", today).is_err());
    }

    #[test]
    fn invalid_component_errors() {
        let today = date(2026, 3, 8);
        assert!(resolve("abc", today).is_err());
    }

    #[test]
    fn dollar_for_day() {
        // last day of current month (march = 31)
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/$", today).unwrap(), date(2026, 3, 31));
    }

    #[test]
    fn caret_for_day() {
        // first day of current month
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/^", today).unwrap(), date(2026, 3, 1));
    }

    #[test]
    fn dollar_for_month_three_components() {
        // last month of year
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/$/~", today).unwrap(), date(2026, 12, 8));
    }

    #[test]
    fn caret_for_month_three_components() {
        // first month of year
        let today = date(2026, 3, 8);
        assert_eq!(resolve("~/^/~", today).unwrap(), date(2026, 1, 8));
    }

    // --- leap year handling for month overflow ---

    #[test]
    fn month_overflow_into_leap_february() {
        // 2024 is a leap year; last day of feb should be 29
        let today = date(2023, 3, 15);
        // year+1, month feb, last day
        assert_eq!(resolve("+1/2/$", today).unwrap(), date(2024, 2, 29));
    }
}
