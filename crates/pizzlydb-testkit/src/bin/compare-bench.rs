//! Compare two benchmark JSON files.
//!
//! ```text
//! compare-bench left.json right.json [tolerance]
//! ```
//!
//! `tolerance` defaults to `0.05`. The process exits with status 1 when a
//! median moved by more than that fraction.

use std::env;
use std::fs;
use std::process::ExitCode;

use pizzlydb_testkit::{compare_reports, report_from_json};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(left_path) = args.next() else {
        eprintln!("usage: compare-bench left.json right.json [tolerance]");
        return ExitCode::from(2);
    };
    let Some(right_path) = args.next() else {
        eprintln!("usage: compare-bench left.json right.json [tolerance]");
        return ExitCode::from(2);
    };
    let tolerance = args
        .next()
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or(0.05);

    let left = match read_report(&left_path) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("{left_path}: {err}");
            return ExitCode::from(2);
        }
    };
    let right = match read_report(&right_path) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("{right_path}: {err}");
            return ExitCode::from(2);
        }
    };

    let notes = compare_reports(&left, &right, tolerance);
    if notes.is_empty() {
        println!("medians agree within {tolerance:.1}%");
        ExitCode::SUCCESS
    } else {
        for note in &notes {
            println!("{note}");
        }
        ExitCode::from(1)
    }
}

fn read_report(path: &str) -> Result<pizzlydb_testkit::BenchReport, String> {
    let bytes = fs::read_to_string(path).map_err(|err| err.to_string())?;
    report_from_json(&bytes).map_err(|err| err.to_string())
}
