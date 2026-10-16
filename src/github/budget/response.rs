//! GraphQL point charges are distinct from transport request counts.
use serde_json::Value;
use std::io;

pub(crate) struct Quota {
    pub cost: u64,
    pub remaining: u64,
    pub reset: u64,
}

pub(crate) fn quota(value: &Value) -> io::Result<Option<Quota>> {
    let Some(limit) = value.pointer("/data/rateLimit") else {
        return Ok(None);
    };
    let number = |key: &str| {
        limit[key]
            .as_u64()
            .ok_or_else(|| io::Error::other("GraphQL quota response malformed"))
    };
    let reset = limit["resetAt"]
        .as_str()
        .and_then(reset_time)
        .ok_or_else(|| io::Error::other("GraphQL reset time malformed"))?;
    Ok(Some(Quota {
        cost: number("cost")?,
        remaining: number("remaining")?,
        reset,
    }))
}

pub(crate) fn exhausted(value: &Value) -> bool {
    value["errors"].as_array().is_some_and(|errors| {
        errors.iter().any(|error| {
            error["type"] == "RATE_LIMITED" || error["extensions"]["code"] == "RATE_LIMITED"
        })
    })
}

fn reset_time(value: &str) -> Option<u64> {
    if value.len() != 20 || !value.is_ascii() {
        return None;
    }
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ];
    if !separators
        .iter()
        .all(|(at, ch)| value.as_bytes()[*at] == *ch)
    {
        return None;
    }
    let year = value[..4].parse::<i64>().ok()?;
    let month = value[5..7].parse::<i64>().ok()?;
    let day = value[8..10].parse::<i64>().ok()?;
    let hour = value[11..13].parse::<i64>().ok()?;
    let minute = value[14..16].parse::<i64>().ok()?;
    let second = value[17..19].parse::<i64>().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 => 28 + i64::from(leap),
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if year < 1970
        || !(1..=days_in_month).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second).ok()
}
