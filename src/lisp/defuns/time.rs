//! `(now-seconds)` / `(window-elapsed)` — wall-clock helpers used
//! by time-driven load profiles — plus `(parse-offset STR)` /
//! `(parse-time-of-day STR)`, the human-time readers the scenario
//! section wrappers use to resolve cue/check times to seconds.

use tulisp::{Error, TulispContext, TulispObject};

use crate::sim::sim_clock::{parse_offset, parse_time_of_day};

pub(super) fn register(ctx: &mut TulispContext) {
    // chrono::Utc::now goes through the same clock_gettime(CLOCK_REALTIME)
    // syscall as std::time::SystemTime::now (both elide leap seconds the
    // same way the kernel does), but using chrono keeps these helpers
    // consistent with the rest of macrocosim's time handling and lets us
    // extend with calendar-aware variants (seconds-since-midnight, etc.)
    // without swapping API later.
    ctx.defun(
        (
            "now-seconds",
            "Return the wall-clock time in seconds since the Unix epoch.\n\n\
             The value is a float. It reads the wall clock in a stepped run \
             too.",
        ),
        || -> f64 {
            let now = chrono::Utc::now();
            now.timestamp() as f64 + now.timestamp_subsec_nanos() as f64 * 1e-9
        },
    );

    ctx.defun(
        (
            "window-elapsed",
            ["window-s"],
            "Return the seconds passed in the current window of WINDOW-S seconds.\n\n\
             The windows follow each other from the Unix epoch, so the value \
             goes from 0 up to WINDOW-S and then starts at 0 again. \
             (window-elapsed 900.0) is the same as (mod (now-seconds) 900.0). \
             A WINDOW-S of 0 or less returns 0. It reads the wall clock.",
        ),
        |window_secs: f64| -> f64 {
            if window_secs <= 0.0 {
                return 0.0;
            }
            let now = chrono::Utc::now();
            let t = now.timestamp() as f64 + now.timestamp_subsec_nanos() as f64 * 1e-9;
            t.rem_euclid(window_secs)
        },
    );

    ctx.defun(
        (
            "parse-offset",
            ["offset"],
            "Return the seconds in OFFSET, a number or a string like \"3min\".\n\n\
             A number, even a negative one, is returned as a float. A string is a number and a unit: \
             ms, s, sec, secs, m, min, mins, h, hr or hrs, as in \"500ms\", \
             \"60s\" or \"2h\". A string with no unit counts as seconds. \
             Signal an error when the string is not in this form, or when it \
             is negative.",
        ),
        |t: TulispObject| -> Result<f64, Error> { time_to_secs(&t, parse_offset, "parse-offset") },
    );

    ctx.defun(
        (
            "parse-time-of-day",
            ["time"],
            "Return the seconds from midnight to TIME, a string \"HH:MM\".\n\n\
             HH is the hour, from 0 to 23, and MM is the minute, from 0 to 59. \
             A number, even a negative one, is returned as a float. Signal an error for any other \
             string.",
        ),
        |t: TulispObject| -> Result<f64, Error> {
            time_to_secs(&t, parse_time_of_day, "parse-time-of-day")
        },
    );

    ctx.defun(
        (
            "resolve-time",
            ["time"],
            "Return TIME in seconds, from a number, an offset or a clock time.\n\n\
             A number, even a negative one, is returned as a float. A string with a colon is a clock \
             time \"HH:MM\", read as parse-time-of-day reads it. Any other \
             string is an offset such as \"60s\", read as parse-offset reads \
             it. The at, check and controller helpers read their times with \
             this. Signal an error for a string in neither form.",
        ),
        |t: TulispObject| -> Result<f64, Error> {
            if t.numberp() {
                return f64::try_from(t);
            }
            let s = String::try_from(t)?;
            let parsed = if s.contains(':') {
                parse_time_of_day(&s)
            } else {
                parse_offset(&s)
            };
            parsed
                .map(|d| d.as_secs_f64())
                .ok_or_else(|| Error::os_error(format!("resolve-time: malformed time {s:?}")))
        },
    );
}

/// Shared body for the two time-parsing defuns: a number is returned
/// verbatim as seconds; a string is run through `parse` (`parse_offset`
/// or `parse_time_of_day`) and errors on a malformed value.
fn time_to_secs(
    t: &TulispObject,
    parse: fn(&str) -> Option<std::time::Duration>,
    who: &str,
) -> Result<f64, Error> {
    if t.numberp() {
        return f64::try_from(t.clone());
    }
    let s = String::try_from(t.clone())?;
    parse(&s)
        .map(|d| d.as_secs_f64())
        .ok_or_else(|| Error::os_error(format!("{who}: malformed time {s:?}")))
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;

    /// tulisp's time functions give these values, as in Emacs; RELEASE_NOTES.md
    /// gives them as examples of what changed with tulisp 0.32.
    #[test]
    fn tulisp_time_functions_work_as_in_emacs() {
        let (cfg, _dir) = config_with("");
        assert_eq!(cfg.eval("(time-add 1 2)").unwrap(), "3");
        assert_eq!(
            cfg.eval("(time-add '(1500000000 . 1000000000) 1)").unwrap(),
            "(5 . 2)"
        );
        assert_eq!(cfg.eval("(time-less-p '(1 . 3) '(1 . 2))").unwrap(), "t");
        assert_eq!(
            cfg.eval("(format-seconds \"%m:%s\" 3661)").unwrap(),
            "\"61:1\""
        );
    }

    fn secs(cfg: &crate::lisp::Config, expr: &str) -> f64 {
        cfg.eval(expr).unwrap().parse().unwrap()
    }

    #[test]
    fn parse_offset_defun_handles_strings_and_numbers() {
        let (cfg, _dir) = config_with("");
        assert_eq!(secs(&cfg, "(parse-offset \"500ms\")"), 0.5);
        assert_eq!(secs(&cfg, "(parse-offset \"3min\")"), 180.0);
        // A bare number rides through as seconds.
        assert_eq!(secs(&cfg, "(parse-offset 90)"), 90.0);
        assert_eq!(secs(&cfg, "(parse-offset 1.5)"), 1.5);
        // Malformed strings error (a scenario-authoring bug).
        assert!(cfg.eval("(parse-offset \"nope\")").is_err());
        assert!(cfg.eval("(parse-offset \"-5s\")").is_err());
    }

    #[test]
    fn parse_time_of_day_defun_resolves_hhmm() {
        let (cfg, _dir) = config_with("");
        assert_eq!(secs(&cfg, "(parse-time-of-day \"00:00\")"), 0.0);
        assert_eq!(secs(&cfg, "(parse-time-of-day \"14:30\")"), 52200.0);
        assert!(cfg.eval("(parse-time-of-day \"24:00\")").is_err());
        assert!(cfg.eval("(parse-time-of-day \"noon\")").is_err());
    }
}
