use crate::{Limits, ThemeError};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum LimitKind {
    FileBytes,
    Files,
    AggregateInputBytes,
    SourceFiles,
    JournalEntries,
    EvidenceManifests,
    RetainedBackups,
    RetainedBackupBytes,
    JsonDepth,
    Tokens,
    ReferenceEdges,
    ReferenceDepth,
    ResolverNodes,
    Profiles,
    ResolverContexts,
    GeneratedBytes,
    GeneratedArtifactBytes,
    Diagnostics,
}

impl LimitKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::FileBytes => "fileBytes",
            Self::Files => "files",
            Self::AggregateInputBytes => "aggregateInputBytes",
            Self::SourceFiles => "sourceFiles",
            Self::JournalEntries => "journalEntries",
            Self::EvidenceManifests => "evidenceManifests",
            Self::RetainedBackups => "retainedBackups",
            Self::RetainedBackupBytes => "retainedBackupBytes",
            Self::JsonDepth => "jsonDepth",
            Self::Tokens => "tokens",
            Self::ReferenceEdges => "referenceEdges",
            Self::ReferenceDepth => "referenceDepth",
            Self::ResolverNodes => "resolverNodes",
            Self::Profiles => "profiles",
            Self::ResolverContexts => "resolverContexts",
            Self::GeneratedBytes => "generatedBytes",
            Self::GeneratedArtifactBytes => "generatedArtifactBytes",
            Self::Diagnostics => "diagnostics",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResourceBudget {
    limits: Limits,
    consumed: BTreeMap<LimitKind, u64>,
}

impl ResourceBudget {
    pub fn new(limits: Limits) -> Result<Self, ThemeError> {
        limits.validate()?;
        Ok(Self {
            limits,
            consumed: BTreeMap::new(),
        })
    }

    pub fn ensure(&self, kind: LimitKind, observed: u64) -> Result<(), ThemeError> {
        let limit = self.limit(kind);
        if observed > limit {
            Err(ThemeError::Limit {
                resource: kind.name(),
                limit,
                observed,
            })
        } else {
            Ok(())
        }
    }

    pub fn consume(&mut self, kind: LimitKind, amount: u64) -> Result<u64, ThemeError> {
        let observed = self
            .consumed(kind)
            .checked_add(amount)
            .ok_or(ThemeError::Limit {
                resource: kind.name(),
                limit: self.limit(kind),
                observed: u64::MAX,
            })?;
        self.ensure(kind, observed)?;
        self.consumed.insert(kind, observed);
        Ok(observed)
    }

    #[must_use]
    pub fn consumed(&self, kind: LimitKind) -> u64 {
        self.consumed.get(&kind).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn limit(&self, kind: LimitKind) -> u64 {
        match kind {
            LimitKind::FileBytes => self.limits.file_bytes,
            LimitKind::Files => u64::from(self.limits.files),
            LimitKind::AggregateInputBytes => self.limits.aggregate_input_bytes,
            LimitKind::SourceFiles => u64::from(self.limits.source_files),
            LimitKind::JournalEntries => u64::from(self.limits.journal_entries),
            LimitKind::EvidenceManifests => u64::from(self.limits.evidence_manifests),
            LimitKind::RetainedBackups => u64::from(self.limits.retained_backups),
            LimitKind::RetainedBackupBytes => self.limits.retained_backup_bytes,
            LimitKind::JsonDepth => u64::from(self.limits.json_depth),
            LimitKind::Tokens => u64::from(self.limits.tokens),
            LimitKind::ReferenceEdges => u64::from(self.limits.reference_edges),
            LimitKind::ReferenceDepth => u64::from(self.limits.reference_depth),
            LimitKind::ResolverNodes => u64::from(self.limits.resolver_nodes),
            LimitKind::Profiles => u64::from(self.limits.profiles),
            LimitKind::ResolverContexts => u64::from(self.limits.resolver_contexts),
            LimitKind::GeneratedBytes => self.limits.generated_bytes,
            LimitKind::GeneratedArtifactBytes => self.limits.generated_artifact_bytes,
            LimitKind::Diagnostics => u64::from(self.limits.diagnostics),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectConfig;

    #[test]
    fn every_counter_accepts_the_limit_and_rejects_one_over() {
        let kinds = [
            LimitKind::FileBytes,
            LimitKind::Files,
            LimitKind::AggregateInputBytes,
            LimitKind::SourceFiles,
            LimitKind::JournalEntries,
            LimitKind::EvidenceManifests,
            LimitKind::RetainedBackups,
            LimitKind::RetainedBackupBytes,
            LimitKind::JsonDepth,
            LimitKind::Tokens,
            LimitKind::ReferenceEdges,
            LimitKind::ReferenceDepth,
            LimitKind::ResolverNodes,
            LimitKind::Profiles,
            LimitKind::ResolverContexts,
            LimitKind::GeneratedBytes,
            LimitKind::GeneratedArtifactBytes,
            LimitKind::Diagnostics,
        ];
        let defaults = [
            2_097_152, 1_024, 67_108_864, 512, 128, 512, 128, 67_108_864, 64, 25_000, 125_000, 64,
            12_500, 128, 128, 16_777_216, 2_097_152, 1_250,
        ];
        let maxima = [
            16_777_216,
            8_192,
            536_870_912,
            4_096,
            1_024,
            4_096,
            1_024,
            536_870_912,
            256,
            200_000,
            1_000_000,
            256,
            100_000,
            1_024,
            1_024,
            134_217_728,
            16_777_216,
            10_000,
        ];
        let budget = ResourceBudget::new(ProjectConfig::default().limits).unwrap();
        let compiled = ResourceBudget::new(crate::COMPILED_LIMITS.clone()).unwrap();
        for ((kind, expected_default), expected_maximum) in
            kinds.into_iter().zip(defaults).zip(maxima)
        {
            assert_eq!(budget.limit(kind), expected_default, "{}", kind.name());
            assert_eq!(compiled.limit(kind), expected_maximum, "{}", kind.name());
        }
        for kind in kinds {
            let limit = budget.limit(kind);
            budget.ensure(kind, limit).unwrap();
            assert!(budget.ensure(kind, limit + 1).is_err(), "{}", kind.name());
        }
    }

    #[test]
    fn consumption_is_checked_before_commit() {
        let mut budget = ResourceBudget::new(ProjectConfig::default().limits).unwrap();
        let limit = budget.limit(LimitKind::Files);
        budget.consume(LimitKind::Files, limit).unwrap();
        assert!(budget.consume(LimitKind::Files, 1).is_err());
        assert_eq!(budget.consumed(LimitKind::Files), limit);
    }
}
