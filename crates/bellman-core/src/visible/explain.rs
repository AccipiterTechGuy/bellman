//! Human-readable schedule explanations.

use crate::visible::cron::parse::{CronSchedule, SpecialSchedule};

/// Explain a parsed 5-field (or special) cron schedule in plain English.
pub fn explain_cron(schedule: &CronSchedule, tz: &str) -> String {
    match schedule {
        CronSchedule::Special(s) => match s {
            SpecialSchedule::Reboot => "at system reboot".into(),
            SpecialSchedule::Yearly => format!("once a year (midnight Jan 1) in {tz}"),
            SpecialSchedule::Monthly => format!("monthly on the 1st at midnight in {tz}"),
            SpecialSchedule::Weekly => format!("weekly on Sunday at midnight in {tz}"),
            SpecialSchedule::Daily => format!("every day at midnight in {tz}"),
            SpecialSchedule::Hourly => format!("every hour at minute 0 in {tz}"),
        },
        CronSchedule::Fields {
            minute,
            hour,
            dom,
            month,
            dow,
        } => {
            let when = describe_time(minute, hour);
            let days = describe_days(dom, dow);
            let months = describe_month(month);
            if months == "every month" {
                format!("{days} {when} ({tz})")
            } else {
                format!("{days} {when}, {months} ({tz})")
            }
        }
    }
}

fn describe_time(minute: &str, hour: &str) -> String {
    if minute == "*" && hour == "*" {
        return "every minute".into();
    }
    if minute.starts_with("*/") && hour == "*" {
        let step = &minute[2..];
        return format!("every {step} minutes");
    }
    if hour.starts_with("*/") && (minute == "0" || minute == "00") {
        let step = &hour[2..];
        return format!("every {step} hours at minute 0");
    }
    if is_single_num(minute) && is_single_num(hour) {
        return format!("at {:02}:{:02}", parse_u(hour), parse_u(minute));
    }
    if is_single_num(hour) && minute == "*" {
        return format!("every minute during hour {hour}");
    }
    format!("at minute={minute} hour={hour}")
}

fn describe_days(dom: &str, dow: &str) -> String {
    let dom_any = dom == "*";
    let dow_any = dow == "*" || dow.is_empty();
    if dom_any && dow_any {
        return "every day".into();
    }
    if dom_any {
        return format!("on {}", pretty_dow(dow));
    }
    if dow_any {
        if is_single_num(dom) {
            return format!("on day {dom} of the month");
        }
        return format!("on days-of-month {dom}");
    }
    format!("on day-of-month {dom} and {}", pretty_dow(dow))
}

fn describe_month(month: &str) -> String {
    if month == "*" {
        "every month".into()
    } else {
        format!("in month(s) {month}")
    }
}

fn pretty_dow(dow: &str) -> String {
    let upper = dow.to_ascii_uppercase();
    // Common ranges
    if upper == "1-5" || upper == "MON-FRI" {
        return "weekdays (Mon–Fri)".into();
    }
    if upper == "0,6" || upper == "6,0" || upper == "SAT,SUN" {
        return "weekends".into();
    }
    format!("weekdays {dow}")
}

fn is_single_num(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_u(s: &str) -> u32 {
    s.parse().unwrap_or(0)
}

/// Explain a systemd OnCalendar / timer description (best-effort).
pub fn explain_systemd(expr: &str) -> String {
    if expr.is_empty() {
        "systemd timer (see unit)".into()
    } else {
        format!("systemd calendar: {expr}")
    }
}

/// Explain an `at` job time.
pub fn explain_at(when: &str) -> String {
    format!("one-shot at job scheduled for {when}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visible::cron::parse::CronSchedule;

    #[test]
    fn weekday_morning() {
        let s = CronSchedule::Fields {
            minute: "0".into(),
            hour: "6".into(),
            dom: "*".into(),
            month: "*".into(),
            dow: "MON-FRI".into(),
        };
        let e = explain_cron(&s, "Europe/Helsinki");
        assert!(e.contains("weekdays"), "{e}");
        assert!(e.contains("06:00"), "{e}");
        assert!(e.contains("Europe/Helsinki"), "{e}");
    }
}
