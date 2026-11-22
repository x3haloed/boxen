//! Inspector readings projected from retained runtime events.
use crate::conversation::utc_minute;

type Counts = (u64, u64);

#[derive(Clone, Copy, Debug)]
pub struct ExchangeUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub at: u64,
    pub runtime: &'static str,
}

impl ExchangeUsage {
    pub fn label(self) -> String {
        let amount =
            |value: Option<u64>| value.map_or("unknown".into(), crate::model::token_amount);
        format!("in {} · out {}", amount(self.input), amount(self.output))
    }

    pub fn source(self) -> String {
        format!("{} · {} UTC", self.runtime, utc_minute(self.at))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TestResult {
    pub passed: u64,
    pub total: u64,
    pub command: &'static str,
    pub at: u64,
}

impl TestResult {
    pub fn observe(command: &str, output: &str, at: u64) -> Option<Self> {
        let mut parts = command.split_whitespace();
        let runner = parts.next()?;
        if parts.next()? != "test" || command.contains([';', '&', '|', '\n', '\r', '>', '<', '`']) {
            return None;
        }
        let (label, parse): (&str, fn(&str) -> Option<Counts>) = match runner {
            "dotnet" => ("dotnet test", dotnet),
            "cargo" => ("cargo test", cargo),
            _ => return None,
        };
        let (passed, total) = output.lines().filter_map(parse).try_fold(
            (0_u64, 0_u64),
            |(passed, total), (next_passed, next_total)| {
                Some((
                    passed.checked_add(next_passed)?,
                    total.checked_add(next_total)?,
                ))
            },
        )?;
        (total != 0).then_some(Self {
            passed,
            total,
            command: label,
            at,
        })
    }
}

fn dotnet(line: &str) -> Option<Counts> {
    let measure = |name| {
        line.split_once(name)?
            .1
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<u64>()
            .ok()
    };
    let passed = measure("Passed:")?;
    let failed = measure("Failed:")?;
    let skipped = measure("Skipped:")?;
    let total = measure("Total:")?;
    (passed.checked_add(failed)?.checked_add(skipped)? == total).then_some((passed, total))
}

fn cargo(line: &str) -> Option<Counts> {
    let (_, report) = line
        .trim()
        .strip_prefix("test result: ")?
        .split_once(". ")?;
    let mut counts = report.split(';');
    let count = |field: &str, suffix: &str| field.trim().strip_suffix(suffix)?.parse::<u64>().ok();
    let passed = count(counts.next()?, " passed")?;
    let failed = count(counts.next()?, " failed")?;
    let ignored = count(counts.next()?, " ignored")?;
    Some((passed, passed.checked_add(failed)?.checked_add(ignored)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_require_a_supported_command_and_consistent_terminal_counts() {
        let dotnet = "Failed! - Failed: 2, Passed: 244, Skipped: 0, Total: 246";
        assert_eq!(
            TestResult::observe("dotnet test", dotnet, 12),
            Some(TestResult {
                passed: 244,
                total: 246,
                command: "dotnet test",
                at: 12
            })
        );
        assert!(TestResult::observe("echo dotnet test", dotnet, 12).is_none());
        assert!(TestResult::observe("dotnet test && echo extra", dotnet, 12).is_none());
        assert!(TestResult::observe("dotnet test", &dotnet.replace("246", "245"), 12).is_none());
        let cargo = "test result: ok. 4 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out";
        let results = TestResult::observe("cargo test --locked", cargo, 12).unwrap();
        assert_eq!((results.passed, results.total), (4, 5));
    }
}
