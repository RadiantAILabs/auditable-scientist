//! The exit codes of the `polaris-scientist` command.

/// How the command ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    /// The command did what it was asked: a campaign `completed` or
    /// `completed_no_winner`, or a report was produced.
    Done,
    /// The work ran and failed: a campaign `failed` or `paper_incomplete`, or
    /// could not run to its end.
    Failed,
    /// The command line, a configuration value, a credential or a file named
    /// on the command line was unusable. Nothing was run.
    Usage,
    /// A campaign was `interrupted` and can be resumed.
    Interrupted,
}

impl Exit {
    /// The process exit status.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Failed => 1,
            Self::Usage => 2,
            Self::Interrupted => 4,
        }
    }

    /// The exit for a campaign outcome name, as written to `outcome.json`.
    ///
    /// Every name other than the successful and interrupted ones counts as a
    /// failure, so an outcome added later is never reported as success.
    #[must_use]
    pub fn of_outcome(outcome: &str) -> Self {
        match outcome {
            "completed" | "completed_no_winner" => Self::Done,
            "interrupted" => Self::Interrupted,
            _ => Self::Failed,
        }
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_campaign_outcome_has_its_documented_exit_code() {
        let expected = [
            ("completed", 0),
            ("completed_no_winner", 0),
            ("failed", 1),
            ("paper_incomplete", 1),
            ("interrupted", 4),
            ("a name nobody has heard of", 1),
            ("", 1),
        ];
        for (outcome, code) in expected {
            assert_eq!(Exit::of_outcome(outcome).code(), code, "{outcome:?}");
        }
    }

    #[test]
    fn the_other_exits_keep_their_numbers() {
        assert_eq!(Exit::Done.code(), 0);
        assert_eq!(Exit::Failed.code(), 1);
        assert_eq!(Exit::Usage.code(), 2);
        assert_eq!(Exit::Interrupted.code(), 4);
    }
}
