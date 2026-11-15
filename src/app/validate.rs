//! Client-side check of a value typed in a cell, by column type. The server
//! stays the authority (it casts every value); this only catches obvious
//! mistakes before staging, with a message in French.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

/// `Err(message)` if `value` cannot be a `data_type`.
pub fn validate(data_type: &str, value: &str) -> Result<(), String> {
    let t = engine_alias(data_type.trim());
    if t.ends_with("[]") {
        return array(value);
    }
    let base = t.split('(').next().unwrap_or(t).trim();
    let v = value.trim();
    match base {
        "smallint" => int(v, i16::MIN as i64, i16::MAX as i64, "smallint"),
        "integer" => int(v, i32::MIN as i64, i32::MAX as i64, "integer"),
        "bigint" => v
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| "entier attendu (bigint)".into()),
        "real" | "double precision" => float(v),
        "numeric" => numeric(v),
        "boolean" => boolean(v),
        "uuid" => uuid(v),
        "json" | "jsonb" => serde_json::from_str::<serde_json::Value>(value)
            .map(|_| ())
            .map_err(|e| format!("JSON invalide : {e}")),
        "date" => date(v),
        b if b.starts_with("timestamp") => timestamp(v),
        b if b.starts_with("time") => time(v),
        _ => Ok(()),
    }
}

/// Type names as reported for query results (`int4`) -> SQL names.
fn engine_alias(t: &str) -> &str {
    match t {
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "timestamptz" => "timestamp with time zone",
        "timetz" => "time with time zone",
        "_int2" | "_int4" | "_int8" | "_text" | "_varchar" | "_numeric" | "_bool" => "array[]",
        other => other,
    }
}

fn int(v: &str, min: i64, max: i64, name: &str) -> Result<(), String> {
    match v.parse::<i64>() {
        Ok(n) if (min..=max).contains(&n) => Ok(()),
        Ok(_) => Err(format!("hors limites pour {name} ({min} .. {max})")),
        Err(_) => Err(format!("entier attendu ({name})")),
    }
}

fn float(v: &str) -> Result<(), String> {
    let special = ["nan", "infinity", "-infinity", "+infinity", "inf", "-inf"];
    if special.contains(&v.to_lowercase().as_str()) || v.parse::<f64>().is_ok() {
        Ok(())
    } else {
        Err("nombre attendu (ex. 3.14, 1e-3, NaN)".into())
    }
}

fn numeric(v: &str) -> Result<(), String> {
    if v.eq_ignore_ascii_case("nan") {
        return Ok(());
    }
    let digits = v.strip_prefix(['-', '+']).unwrap_or(v);
    let (mantissa, exp) = match digits.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (digits, None),
    };
    let mut parts = mantissa.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let ok = !(int_part.is_empty() && frac.is_empty())
        && int_part.chars().all(|c| c.is_ascii_digit())
        && frac.chars().all(|c| c.is_ascii_digit())
        && exp.is_none_or(|e| {
            e.strip_prefix(['-', '+'])
                .unwrap_or(e)
                .parse::<u32>()
                .is_ok()
        });
    if ok {
        Ok(())
    } else {
        Err("nombre décimal attendu (ex. 12.50)".into())
    }
}

fn boolean(v: &str) -> Result<(), String> {
    let ok = [
        "t", "f", "true", "false", "y", "n", "yes", "no", "on", "off", "1", "0",
    ];
    if ok.contains(&v.to_lowercase().as_str()) {
        Ok(())
    } else {
        Err("booléen attendu (true / false)".into())
    }
}

fn uuid(v: &str) -> Result<(), String> {
    let hex: String = v
        .trim_start_matches('{')
        .trim_end_matches('}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("UUID attendu (ex. 6d34dcb9-2e0f-43ff-8aeb-814b4d033081)".into())
    }
}

/// Words PostgreSQL accepts for dates and timestamps.
fn special_date(v: &str) -> bool {
    [
        "now",
        "today",
        "tomorrow",
        "yesterday",
        "infinity",
        "-infinity",
        "epoch",
    ]
    .contains(&v.to_lowercase().as_str())
}

fn date(v: &str) -> Result<(), String> {
    if special_date(v) || NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() {
        Ok(())
    } else {
        Err("date attendue (AAAA-MM-JJ)".into())
    }
}

fn time(v: &str) -> Result<(), String> {
    // Optional time zone suffix (timetz): checked by the server.
    let core = v.split(['+', 'Z']).next().unwrap_or(v);
    let core = if core.matches('-').count() > 0 && !core.starts_with('-') {
        core.split('-').next().unwrap_or(core)
    } else {
        core
    };
    let ok = ["%H:%M:%S%.f", "%H:%M"]
        .iter()
        .any(|f| NaiveTime::parse_from_str(core.trim(), f).is_ok());
    if ok || v.eq_ignore_ascii_case("now") {
        Ok(())
    } else {
        Err("heure attendue (HH:MM[:SS])".into())
    }
}

fn timestamp(v: &str) -> Result<(), String> {
    if special_date(v) || date(v).is_ok() {
        return Ok(());
    }
    // Strip a time zone (+02, +02:00, Z, or a name after a space).
    let mut core = v.trim().replacen('T', " ", 1);
    if let Some(z) = core.strip_suffix('Z') {
        core = z.to_string();
    }
    if let Some(pos) = core.rfind(['+', '-'])
        && pos > 10
    {
        core.truncate(pos);
    }
    let parts: Vec<&str> = core.split_whitespace().collect();
    let candidate = parts.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    let ok = ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M"]
        .iter()
        .any(|f| NaiveDateTime::parse_from_str(&candidate, f).is_ok());
    if ok {
        Ok(())
    } else {
        Err("horodatage attendu (AAAA-MM-JJ HH:MM[:SS][+TZ])".into())
    }
}

fn array(v: &str) -> Result<(), String> {
    let v = v.trim();
    if v.starts_with('{') && v.ends_with('}') {
        Ok(())
    } else {
        Err("tableau attendu, au format {a,b,c}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert!(validate("integer", "42").is_ok());
        assert!(validate("integer", " -7 ").is_ok());
        assert!(validate("integer", "4.2").is_err());
        assert!(
            validate("smallint", "40000")
                .unwrap_err()
                .contains("hors limites")
        );
        assert!(validate("bigint", "9223372036854775807").is_ok());
    }

    #[test]
    fn numbers() {
        assert!(validate("numeric(12,2)", "12.50").is_ok());
        assert!(validate("numeric", "-.5").is_ok());
        assert!(validate("numeric", "1e5").is_ok());
        assert!(validate("numeric", "12,50").is_err());
        assert!(validate("double precision", "NaN").is_ok());
        assert!(validate("real", "abc").is_err());
    }

    #[test]
    fn booleans_uuids_json() {
        assert!(validate("boolean", "TRUE").is_ok());
        assert!(validate("boolean", "oui").is_err());
        assert!(validate("uuid", "6d34dcb9-2e0f-43ff-8aeb-814b4d033081").is_ok());
        assert!(validate("uuid", "6d34dcb9").is_err());
        assert!(validate("jsonb", "{\"a\": [1, 2]}").is_ok());
        assert!(
            validate("json", "{a: 1}")
                .unwrap_err()
                .starts_with("JSON invalide")
        );
    }

    #[test]
    fn dates_and_times() {
        assert!(validate("date", "2024-02-29").is_ok());
        assert!(validate("date", "2023-02-29").is_err());
        assert!(validate("date", "today").is_ok());
        assert!(validate("timestamp with time zone", "2024-05-01 12:30:00+02").is_ok());
        assert!(validate("timestamp without time zone", "2024-05-01T12:30:00.123").is_ok());
        assert!(
            validate(
                "timestamp with time zone",
                "2024-05-01 12:30:00 Europe/Paris"
            )
            .is_ok()
        );
        assert!(validate("timestamp with time zone", "hier").is_err());
        assert!(validate("time without time zone", "08:15").is_ok());
        assert!(validate("time with time zone", "08:15:00+02").is_ok());
        assert!(validate("time without time zone", "25:00").is_err());
    }

    #[test]
    fn others_are_left_to_the_server() {
        assert!(validate("text", "anything").is_ok());
        assert!(validate("inet", "not an ip").is_ok());
        assert!(validate("integer[]", "{1,2}").is_ok());
        assert!(validate("integer[]", "1,2").is_err());
    }
}
