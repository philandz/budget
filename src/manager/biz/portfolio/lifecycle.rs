//! Asset lifecycle state machine.
//!
//! Encodes the allowed transitions documented in spec §6. Any
//! transition not listed returns `LifecycleError::Illegal`.

use super::AssetStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Close,
    Mature,
    Sell,
    Archive,
    RollOver,
    Withdraw,
    EarlyClose,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("illegal transition from {from:?} via {transition:?}")]
    Illegal {
        from: AssetStatus,
        transition: Transition,
    },
}

/// Returns the new status if the transition is allowed.
pub fn next_status(
    from: AssetStatus,
    transition: Transition,
) -> Result<AssetStatus, LifecycleError> {
    use AssetStatus::*;
    use Transition::*;
    let to = match (from, transition) {
        // ACTIVE can do anything.
        (Active, Close) => Closed,
        (Active, Mature) => Matured,
        (Active, Sell) => Sold,
        (Active, Archive) => Archived,
        (Active, RollOver) => RolledOver,
        (Active, Withdraw) => Withdrawn,
        (Active, EarlyClose) => EarlyClosed,

        // MATURED can be rolled over, withdrawn, or archived.
        (Matured, RollOver) => RolledOver,
        (Matured, Withdraw) => Withdrawn,
        (Matured, Archive) => Archived,

        // CLOSED is terminal except archive.
        (Closed, Archive) => Archived,

        // SOLD is terminal except archive.
        (Sold, Archive) => Archived,

        // ROLLED_OVER, WITHDRAWN, EARLY_CLOSED → archived only.
        (RolledOver, Archive) => Archived,
        (Withdrawn, Archive) => Archived,
        (EarlyClosed, Archive) => Archived,

        // ARCHIVED is terminal.
        (Archived, _) => {
            return Err(LifecycleError::Illegal { from, transition });
        }

        // Anything else is illegal.
        _ => {
            return Err(LifecycleError::Illegal { from, transition });
        }
    };
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_can_close_mature_sell_archive() {
        assert_eq!(
            next_status(AssetStatus::Active, Transition::Close).unwrap(),
            AssetStatus::Closed
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::Mature).unwrap(),
            AssetStatus::Matured
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::Sell).unwrap(),
            AssetStatus::Sold
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::Archive).unwrap(),
            AssetStatus::Archived
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::RollOver).unwrap(),
            AssetStatus::RolledOver
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::Withdraw).unwrap(),
            AssetStatus::Withdrawn
        );
        assert_eq!(
            next_status(AssetStatus::Active, Transition::EarlyClose).unwrap(),
            AssetStatus::EarlyClosed
        );
    }

    #[test]
    fn matured_can_only_rollover_withdraw_or_archive() {
        assert!(next_status(AssetStatus::Matured, Transition::RollOver).is_ok());
        assert!(next_status(AssetStatus::Matured, Transition::Withdraw).is_ok());
        assert!(next_status(AssetStatus::Matured, Transition::Archive).is_ok());
        assert!(next_status(AssetStatus::Matured, Transition::Sell).is_err());
        assert!(next_status(AssetStatus::Matured, Transition::Close).is_err());
    }

    #[test]
    fn closed_can_only_archive() {
        assert!(next_status(AssetStatus::Closed, Transition::Archive).is_ok());
        assert!(next_status(AssetStatus::Closed, Transition::Sell).is_err());
        assert!(next_status(AssetStatus::Closed, Transition::Mature).is_err());
    }

    #[test]
    fn sold_can_only_archive() {
        assert!(next_status(AssetStatus::Sold, Transition::Archive).is_ok());
        assert!(next_status(AssetStatus::Sold, Transition::Close).is_err());
    }

    #[test]
    fn archived_is_terminal() {
        for t in [
            Transition::Close,
            Transition::Mature,
            Transition::Sell,
            Transition::Archive,
            Transition::RollOver,
            Transition::Withdraw,
            Transition::EarlyClose,
        ] {
            assert!(next_status(AssetStatus::Archived, t).is_err());
        }
    }

    #[test]
    fn rolled_over_withdrawn_early_closed_can_only_archive() {
        for s in [
            AssetStatus::RolledOver,
            AssetStatus::Withdrawn,
            AssetStatus::EarlyClosed,
        ] {
            assert!(next_status(s, Transition::Archive).is_ok());
            assert!(next_status(s, Transition::Close).is_err());
            assert!(next_status(s, Transition::Sell).is_err());
        }
    }
}
