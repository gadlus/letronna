//! Holds the bench's numbers to `bench/baseline.json`.
//!
//! The core's bench and the terminal's both include this file, so they keep
//! to one baseline and one rule. The file keeps a baseline per platform,
//! under `<os>-<arch>`, since a time means something only on the machine it
//! was taken on: a platform with none is measured and printed, never failed.
//! Every measure is a cost, lower being better, and one that comes out more
//! than `SLACK` over its baseline fails the bench. With
//! `LETRONNA_BENCH_RECORD=1` what was measured becomes this platform's
//! baseline instead, leaving the other platforms and any notes as they are.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// How far over its baseline a measure may come before the bench fails: the
/// 20% the roadmap allows, above what a quiet machine varies by at the median.
const SLACK: f64 = 1.2;
/// Times each measure is taken, the median counting: one run slowed by
/// something else on the machine does not move it, and the whole bench
/// still takes seconds.
const RUNS: usize = 7;

/// The median of `RUNS` measures that `take` makes.
pub fn median(mut take: impl FnMut() -> f64) -> f64 {
    let mut runs: Vec<f64> = (0..RUNS).map(|_| take()).collect();
    runs.sort_by(f64::total_cmp);
    runs[RUNS / 2]
}

/// Holds `measured` to this platform's baseline, or makes it the baseline
/// when `LETRONNA_BENCH_RECORD` is set; Err names each measure that slowed.
pub fn check(measured: &[(&str, f64)]) -> Result<(), String> {
    let path = file();
    let mut all: Map<String, Value> = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} will not read: {e}", path.display()))?,
        Err(_) => Map::new(),
    };
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    // Said with the numbers, since a machine busier than it was when the
    // baseline was taken is slower at everything, by more than 20% here
    // once other builds held every core.
    if let Some(load) = load() {
        let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
        println!("  load {load:.1} over the last minute, on {cores} cores");
    }
    if std::env::var_os("LETRONNA_BENCH_RECORD").is_some() {
        let entry = all
            .entry(platform.as_str())
            .or_insert_with(|| Map::new().into());
        let Some(entry) = entry.as_object_mut() else {
            return Err(format!("{platform} in {} is not an object", path.display()));
        };
        for (name, value) in measured {
            // Three places are finer than any run repeats to.
            let value = (value * 1000.0).round() / 1000.0;
            println!("  {name}: {value} recorded");
            entry.insert((*name).into(), value.into());
        }
        let text = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())? + "\n";
        return std::fs::write(&path, text)
            .map_err(|e| format!("could not write {}: {e}", path.display()));
    }
    let baseline = all.get(&platform).and_then(Value::as_object);
    held_to(baseline, &platform, measured)
}

/// Holds `measured` to `baseline`, saying how each compares; Err names each
/// measure more than `SLACK` over its baseline.
fn held_to(
    baseline: Option<&Map<String, Value>>,
    platform: &str,
    measured: &[(&str, f64)],
) -> Result<(), String> {
    let mut slower = Vec::new();
    for (name, value) in measured {
        let Some(was) = baseline.and_then(|b| b.get(*name)).and_then(Value::as_f64) else {
            println!("  {name}: {value:.3} (no baseline for {platform})");
            continue;
        };
        let change = (value / was - 1.0) * 100.0;
        println!("  {name}: {value:.3} (baseline {was}, {change:+.0}%)");
        if *value > was * SLACK {
            slower.push(format!("{name} {value:.3} against {was}"));
        }
    }
    if slower.is_empty() {
        return Ok(());
    }
    Err(format!(
        "more than 20% over the baseline: {}",
        slower.join(", ")
    ))
}

/// The load average over the last minute.
#[cfg(unix)]
fn load() -> Option<f64> {
    let mut loads = [0.0];
    // SAFETY: one sample asked for, into a buffer of one.
    let taken = unsafe { libc::getloadavg(loads.as_mut_ptr(), 1) };
    (taken == 1).then_some(loads[0])
}

/// No load average: Windows keeps none.
#[cfg(not(unix))]
fn load() -> Option<f64> {
    None
}

/// The baseline file, found from the crate that includes this one.
fn file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/baseline.json")
}

#[cfg(test)]
mod tests {
    use super::held_to;
    use serde_json::json;

    #[test]
    fn only_a_measure_more_than_a_fifth_over_its_baseline_fails() {
        let baseline = json!({"slower": 10.0, "noisier": 10.0, "faster": 10.0});
        let measured = [("slower", 12.5), ("noisier", 11.9), ("faster", 2.0)];
        let failed = held_to(baseline.as_object(), "here", &measured).unwrap_err();
        assert!(failed.contains("slower 12.500 against 10"), "{failed}");
        assert!(!failed.contains("noisier") && !failed.contains("faster"));
    }

    #[test]
    fn a_measure_or_a_platform_with_no_baseline_passes() {
        let baseline = json!({"other": 1.0});
        assert!(held_to(baseline.as_object(), "here", &[("new", 99.0)]).is_ok());
        assert!(held_to(None, "elsewhere", &[("new", 99.0)]).is_ok());
    }
}
