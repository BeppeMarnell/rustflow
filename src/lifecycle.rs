#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn can_cancel(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::RunStatus;

    #[test]
    fn only_active_runs_can_be_cancelled() {
        assert!(RunStatus::Queued.can_cancel());
        assert!(RunStatus::Running.can_cancel());
        for status in [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert!(!status.can_cancel());
        }
        assert_eq!(RunStatus::parse("cancelled"), Some(RunStatus::Cancelled));
        assert_eq!(RunStatus::parse("unknown"), None);
    }
}
