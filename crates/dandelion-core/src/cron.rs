//! C7 Cron: SQLite jobs + in-process ticker. No second agent loop.

use crate::store::CronJobRow;
use crate::{Error, Result};
use chrono::{DateTime, Duration, Local, Utc};
use croner::Cron;
use std::str::FromStr;

pub fn parse_next(expr: &str, from: DateTime<Local>) -> Result<DateTime<Local>> {
    let expr = expr.trim();
    if let Some(d) = parse_interval(expr)? {
        return Ok(from + d);
    }
    let cron = Cron::from_str(expr)
        .map_err(|err| Error::Invalid(format!("invalid cron expr {expr:?}: {err}")))?;
    cron.find_next_occurrence(&from, false)
        .map_err(|err| Error::Invalid(format!("cron next failed: {err}")))
}

pub fn parse_interval(expr: &str) -> Result<Option<Duration>> {
    let s = expr.trim();
    if s.is_empty() {
        return Err(Error::Invalid("cron expr must not be empty".into()));
    }
    let bytes = s.as_bytes();
    let last = *bytes.last().unwrap();
    let is_interval = last == b's' || last == b'm' || last == b'h' || last == b'd';
    if !is_interval || s.contains(' ') {
        return Ok(None);
    }
    let (n, unit) = s.split_at(s.len() - 1);
    let n: i64 = n
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid interval {expr:?}")))?;
    if n <= 0 {
        return Err(Error::Invalid("interval must be > 0".into()));
    }
    let d = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => return Ok(None),
    };
    Ok(Some(d))
}

pub fn rfc3339(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

pub fn next_rfc3339(expr: &str, from_utc: DateTime<Utc>) -> Result<String> {
    let local = from_utc.with_timezone(&Local);
    let next = parse_next(expr, local)?;
    Ok(next.with_timezone(&Utc).to_rfc3339())
}

pub fn validate_expr(expr: &str) -> Result<()> {
    let _ = parse_next(expr, Local::now())?;
    Ok(())
}

pub fn new_job(
    expr: &str,
    prompt: &str,
    workspace: &str,
    model_ref: Option<String>,
    missed_policy: &str,
) -> Result<CronJobRow> {
    if prompt.trim().is_empty() {
        return Err(Error::Invalid("cron prompt must not be empty".into()));
    }
    let policy = match missed_policy {
        "" | "skip" => "skip",
        "run_once" => "run_once",
        other => {
            return Err(Error::Invalid(format!(
                "missed_policy must be skip|run_once (got {other})"
            )))
        }
    };
    validate_expr(expr)?;
    let now = Utc::now();
    Ok(CronJobRow {
        id: format!("cron_{}", uuid::Uuid::new_v4().simple()),
        expr: expr.trim().to_string(),
        prompt: prompt.to_string(),
        workspace: workspace.to_string(),
        model_ref,
        enabled: true,
        last_run: None,
        next_run: next_rfc3339(expr, now)?,
        missed_policy: policy.into(),
        created_at: now.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_and_five_field_cron() {
        let from = Local::now();
        let n = parse_next("30m", from).unwrap();
        assert!(n > from);
        let two_h = parse_next("2h", from).unwrap();
        assert!(two_h > from);
        assert!(validate_expr("0 9 * * *").is_ok());
        assert!(validate_expr("nope").is_err());
        assert!(validate_expr("").is_err());
    }

    #[test]
    fn new_job_validates_prompt_and_policy() {
        assert!(new_job("30m", "", ".", None, "skip").is_err());
        assert!(new_job("30m", "ping", ".", None, "catch_up").is_err());
        let job = new_job("30m", "ping", "/ws", Some("m".into()), "run_once").unwrap();
        assert!(job.id.starts_with("cron_"));
        assert!(job.enabled);
        assert_eq!(job.missed_policy, "run_once");
        assert_eq!(job.prompt, "ping");
        assert!(!job.next_run.is_empty());
        let skipped = new_job("30m", "ping", ".", None, "").unwrap();
        assert_eq!(skipped.missed_policy, "skip");
        assert_eq!(parse_interval("2h").unwrap(), Some(Duration::hours(2)));
        assert!(parse_interval("0m").is_err());
    }
}
