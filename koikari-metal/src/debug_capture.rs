//! Validated debug capture options, independent of Metal and game resources.

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DebugCapture {
    pub scenario: Option<String>,
    pub label: Option<String>,
    pub line: Option<usize>,
    pub advance: usize,
    /// Zero-based internally; the command line is one-based like the UI.
    pub choice: Option<usize>,
    pub replay: bool,
    pub texture_budget: usize,
    /// Preserve the original data/patch resource profile and legacy saves.
    pub base_only: bool,
}

impl DebugCapture {
    pub(crate) fn parse(args: &[String], default_budget: usize) -> Result<Self, String> {
        let scenario = value(args, "--scenario")?;
        let label = value(args, "--label")?;
        let line = number(args, "--line")?;
        let advance = number(args, "--advance")?.unwrap_or(0);
        let choice = number(args, "--choice")?
            .map(|n| {
                n.checked_sub(1)
                    .ok_or("--choice is one-based (1 or greater)".to_owned())
            })
            .transpose()?;
        let replay = args.iter().any(|arg| arg == "--capture-replay");
        let texture_budget = match number(args, "--texture-budget-mib")? {
            Some(mib) => mib
                .checked_mul(1024 * 1024)
                .ok_or("--texture-budget-mib overflows address space".to_owned())?,
            None => default_budget,
        };
        if label.is_some() && scenario.is_none() {
            return Err("--label requires --scenario".into());
        }
        if line.is_some() && scenario.is_none() {
            return Err(
                "--line requires --scenario (the target is a source line in that scenario)".into(),
            );
        }
        if line == Some(0) {
            return Err("--line is one-based (1 or greater)".into());
        }
        if replay && !args.iter().any(|arg| arg == "--capture") {
            return Err("--capture-replay requires --capture <png>".into());
        }
        if args.iter().any(|arg| arg == "--title")
            && (scenario.is_some()
                || label.is_some()
                || line.is_some()
                || choice.is_some()
                || replay)
        {
            return Err("--title cannot be combined with scenario replay options".into());
        }
        Ok(Self {
            scenario,
            label,
            line,
            advance,
            choice,
            replay,
            texture_budget,
            base_only: args.iter().any(|arg| arg == "--base-only"),
        })
    }

    pub(crate) fn starts_game(&self) -> bool {
        self.scenario.is_some() || self.advance > 0 || self.choice.is_some()
    }
}

fn value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let mut positions = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == flag);
    let Some((position, _)) = positions.next() else {
        return Ok(None);
    };
    if positions.next().is_some() {
        return Err(format!("{flag} may only be specified once"));
    }
    args.get(position + 1)
        .filter(|arg| !arg.starts_with("--") && !arg.is_empty())
        .cloned()
        .map(Some)
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn number(args: &[String], flag: &str) -> Result<Option<usize>, String> {
    value(args, flag)?
        .map(|value| {
            value
                .parse()
                .map_err(|_| format!("{flag} expects a non-negative integer, received {value:?}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Result<DebugCapture, String> {
        DebugCapture::parse(
            &values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            768 * 1024 * 1024,
        )
    }

    #[test]
    fn parses_scenario_line_replay_and_one_based_choice() {
        let options = parse(&[
            "--capture",
            "frame.png",
            "--scenario",
            "scenario/main/0_2.ks",
            "--line",
            "306",
            "--advance",
            "120",
            "--choice",
            "2",
            "--capture-replay",
            "--texture-budget-mib",
            "8",
        ])
        .unwrap();
        assert_eq!(options.line, Some(306));
        assert_eq!(options.advance, 120);
        assert_eq!(options.choice, Some(1));
        assert_eq!(options.texture_budget, 8 * 1024 * 1024);
        assert!(options.replay && options.starts_game());
    }

    #[test]
    fn rejects_ambiguous_missing_or_out_of_range_values() {
        for args in [
            vec!["--choice", "0"],
            vec!["--choice"],
            vec!["--advance", "--capture"],
            vec!["--advance", "-1"],
            vec!["--advance", "1", "--advance", "2"],
            vec!["--texture-budget-mib", "18446744073709551615"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn validates_related_flags_and_defaults() {
        for args in [
            vec!["--label", "*start"],
            vec!["--line", "306"],
            vec!["--scenario", "0_1", "--line", "0"],
            vec!["--capture-replay"],
            vec!["--title", "--scenario", "0_1"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
        assert!(!parse(&[]).unwrap().starts_game());
        assert!(!parse(&[]).unwrap().base_only);
        assert!(parse(&["--base-only"]).unwrap().base_only);
        assert_eq!(parse(&[]).unwrap().texture_budget, 768 * 1024 * 1024);
        // Zero deliberately forces eviction while retaining the live frame.
        assert_eq!(
            parse(&["--texture-budget-mib", "0"])
                .unwrap()
                .texture_budget,
            0
        );
        assert_eq!(
            parse(&["--scenario", "0_1", "--label", "*start"])
                .unwrap()
                .label
                .as_deref(),
            Some("*start")
        );
    }
}
