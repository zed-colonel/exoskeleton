//! Relationship Ledger trait and in-memory implementation.
//!
//! The Relationship Ledger is an append-only durable store for relational signals
//! (I8: durable and explicit). The `RelationshipSnapshot` is compiled from the
//! ledger every tick (I5: compiled, not accumulated). Records are never modified
//! or deleted (IBP §4.1).

use std::sync::Mutex;

use exoskeleton_core::{ExoError, LedgerEntryId, PrincipalId, RelationshipRecord, TickId};

/// Append-only durable store for relationship records (I8: durable and explicit).
///
/// The ledger is the source of truth for all relationship state. The
/// `RelationshipSnapshot` is compiled from the ledger every tick (I5: compiled,
/// not accumulated). Records are never modified or deleted (IBP §4.1).
pub trait RelationshipLedger: Send + Sync {
    /// Append a new record to the ledger. Returns the entry's LedgerEntryId.
    ///
    /// The record's `id` field should already be populated. If a record with
    /// the same ID already exists, returns `ExoError::Storage`.
    /// Writes are atomic — crash safety guaranteed by the storage layer.
    fn append(&self, record: &RelationshipRecord) -> Result<LedgerEntryId, ExoError>;

    /// Retrieve all records for a specific principal, newest first.
    fn for_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<RelationshipRecord>, ExoError>;

    /// Retrieve the N most recent records across all principals, newest first.
    fn recent(&self, limit: usize) -> Result<Vec<RelationshipRecord>, ExoError>;

    /// Retrieve all records since (and including) the given tick, oldest first.
    ///
    /// Used by the snapshot compiler to build incremental views and by
    /// the Align step to find recent relationship context.
    fn since_tick(&self, tick_id: TickId) -> Result<Vec<RelationshipRecord>, ExoError>;

    /// List all distinct principal IDs that have records in the ledger.
    fn distinct_principals(&self) -> Result<Vec<PrincipalId>, ExoError>;

    /// Count total records in the ledger.
    fn count(&self) -> Result<u64, ExoError>;
}

/// In-memory implementation of `RelationshipLedger` for unit testing.
///
/// Uses `Mutex<Vec<RelationshipRecord>>` for fast unit testing. Same pattern
/// as `InMemoryThreadStore` from Sprint 6.
pub struct InMemoryRelationshipLedger {
    records: Mutex<Vec<RelationshipRecord>>,
}

impl InMemoryRelationshipLedger {
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryRelationshipLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl RelationshipLedger for InMemoryRelationshipLedger {
    fn append(&self, record: &RelationshipRecord) -> Result<LedgerEntryId, ExoError> {
        let mut records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        // Check for duplicate ID
        if records.iter().any(|r| r.id == record.id) {
            return Err(ExoError::Storage(format!(
                "relationship record already exists with id {}",
                record.id
            )));
        }

        records.push(record.clone());
        Ok(record.id)
    }

    fn for_principal(
        &self,
        principal_id: PrincipalId,
        limit: usize,
    ) -> Result<Vec<RelationshipRecord>, ExoError> {
        let records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut filtered: Vec<_> = records
            .iter()
            .filter(|r| r.principal_id == principal_id)
            .cloned()
            .collect();

        // Newest first (reverse chronological)
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        filtered.truncate(limit);
        Ok(filtered)
    }

    fn recent(&self, limit: usize) -> Result<Vec<RelationshipRecord>, ExoError> {
        let records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut sorted: Vec<_> = records.iter().cloned().collect();
        sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        sorted.truncate(limit);
        Ok(sorted)
    }

    fn since_tick(&self, tick_id: TickId) -> Result<Vec<RelationshipRecord>, ExoError> {
        let records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        // Find the earliest timestamp for the given tick_id
        let min_timestamp = records
            .iter()
            .filter(|r| r.tick_id == tick_id)
            .map(|r| r.timestamp)
            .min();

        match min_timestamp {
            Some(ts) => {
                let mut result: Vec<_> = records
                    .iter()
                    .filter(|r| r.timestamp >= ts)
                    .cloned()
                    .collect();
                // Oldest first
                result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                Ok(result)
            }
            None => Ok(Vec::new()),
        }
    }

    fn distinct_principals(&self) -> Result<Vec<PrincipalId>, ExoError> {
        let records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;

        let mut principals: Vec<PrincipalId> = records.iter().map(|r| r.principal_id).collect();
        principals.sort();
        principals.dedup();
        Ok(principals)
    }

    fn count(&self) -> Result<u64, ExoError> {
        let records = self
            .records
            .lock()
            .map_err(|e| ExoError::Storage(format!("lock poisoned: {e}")))?;
        Ok(records.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use exoskeleton_core::{ArtifactId, RelationalSignalType};

    use super::*;

    fn make_record(
        principal_id: PrincipalId,
        signal_type: RelationalSignalType,
    ) -> RelationshipRecord {
        RelationshipRecord {
            id: LedgerEntryId::new(),
            principal_id,
            signal_type,
            content_ref: ArtifactId::from_content(b"test-signal"),
            tick_id: TickId::new(),
            timestamp: Utc::now(),
            metadata: Default::default(),
        }
    }

    // ── T-1: RelationshipLedger Trait + InMemory ──

    #[test]
    fn append_and_retrieve() {
        let ledger = InMemoryRelationshipLedger::new();
        let principal = PrincipalId::new();
        let record = make_record(principal, RelationalSignalType::TrustUpdate);
        let id = ledger.append(&record).unwrap();
        assert_eq!(id, record.id);

        let recent = ledger.recent(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, record.id);
    }

    #[test]
    fn for_principal_filters_by_principal() {
        let ledger = InMemoryRelationshipLedger::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        ledger
            .append(&make_record(p1, RelationalSignalType::TrustUpdate))
            .unwrap();
        ledger
            .append(&make_record(p2, RelationalSignalType::FeedbackReceived))
            .unwrap();
        ledger
            .append(&make_record(p1, RelationalSignalType::CommitmentMade))
            .unwrap();

        let p1_records = ledger.for_principal(p1, 10).unwrap();
        assert_eq!(p1_records.len(), 2);
        for r in &p1_records {
            assert_eq!(r.principal_id, p1);
        }

        let p2_records = ledger.for_principal(p2, 10).unwrap();
        assert_eq!(p2_records.len(), 1);
        assert_eq!(p2_records[0].principal_id, p2);
    }

    #[test]
    fn recent_returns_newest_first_with_limit() {
        let ledger = InMemoryRelationshipLedger::new();
        let principal = PrincipalId::new();
        let ts_base = Utc::now();

        for i in 0..5 {
            let mut record = make_record(principal, RelationalSignalType::TrustUpdate);
            record.timestamp = ts_base + chrono::Duration::seconds(i);
            ledger.append(&record).unwrap();
        }

        let recent = ledger.recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent[0].timestamp >= recent[1].timestamp);
        assert!(recent[1].timestamp >= recent[2].timestamp);
    }

    #[test]
    fn since_tick_returns_records_from_tick_onward() {
        let ledger = InMemoryRelationshipLedger::new();
        let principal = PrincipalId::new();
        let tick1 = TickId::new();
        let tick2 = TickId::new();
        let tick3 = TickId::new();
        let ts_base = Utc::now();

        // Records for tick1 (earliest)
        let mut r1 = make_record(principal, RelationalSignalType::TrustUpdate);
        r1.tick_id = tick1;
        r1.timestamp = ts_base;
        ledger.append(&r1).unwrap();

        // Records for tick2 (middle)
        let mut r2 = make_record(principal, RelationalSignalType::CommitmentMade);
        r2.tick_id = tick2;
        r2.timestamp = ts_base + chrono::Duration::seconds(10);
        ledger.append(&r2).unwrap();

        // Records for tick3 (latest)
        let mut r3 = make_record(principal, RelationalSignalType::FeedbackReceived);
        r3.tick_id = tick3;
        r3.timestamp = ts_base + chrono::Duration::seconds(20);
        ledger.append(&r3).unwrap();

        // since_tick(tick2) should return tick2 and tick3 records, oldest first
        let since = ledger.since_tick(tick2).unwrap();
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].tick_id, tick2);
        assert_eq!(since[1].tick_id, tick3);
    }

    #[test]
    fn since_tick_unknown_returns_empty() {
        let ledger = InMemoryRelationshipLedger::new();
        let result = ledger.since_tick(TickId::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn distinct_principals_returns_unique_ids() {
        let ledger = InMemoryRelationshipLedger::new();
        let p1 = PrincipalId::new();
        let p2 = PrincipalId::new();

        ledger
            .append(&make_record(p1, RelationalSignalType::TrustUpdate))
            .unwrap();
        ledger
            .append(&make_record(p1, RelationalSignalType::CommitmentMade))
            .unwrap();
        ledger
            .append(&make_record(p2, RelationalSignalType::FeedbackReceived))
            .unwrap();

        let principals = ledger.distinct_principals().unwrap();
        assert_eq!(principals.len(), 2);
        assert!(principals.contains(&p1));
        assert!(principals.contains(&p2));
    }

    #[test]
    fn count_reflects_total_records() {
        let ledger = InMemoryRelationshipLedger::new();
        assert_eq!(ledger.count().unwrap(), 0);

        let p = PrincipalId::new();
        ledger
            .append(&make_record(p, RelationalSignalType::TrustUpdate))
            .unwrap();
        assert_eq!(ledger.count().unwrap(), 1);

        ledger
            .append(&make_record(p, RelationalSignalType::CommitmentMade))
            .unwrap();
        assert_eq!(ledger.count().unwrap(), 2);
    }

    #[test]
    fn append_rejects_duplicate_id() {
        let ledger = InMemoryRelationshipLedger::new();
        let record = make_record(PrincipalId::new(), RelationalSignalType::TrustUpdate);
        ledger.append(&record).unwrap();

        let err = ledger.append(&record).unwrap_err();
        assert!(
            matches!(err, ExoError::Storage(ref msg) if msg.contains("already exists")),
            "Expected duplicate rejection, got: {err}"
        );
    }

    #[test]
    fn empty_ledger_returns_empty_results() {
        let ledger = InMemoryRelationshipLedger::new();
        assert!(ledger.recent(10).unwrap().is_empty());
        assert!(ledger
            .for_principal(PrincipalId::new(), 10)
            .unwrap()
            .is_empty());
        assert!(ledger.since_tick(TickId::new()).unwrap().is_empty());
        assert!(ledger.distinct_principals().unwrap().is_empty());
        assert_eq!(ledger.count().unwrap(), 0);
    }
}
