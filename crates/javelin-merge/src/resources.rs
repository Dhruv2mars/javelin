use std::fmt;

/// Fixed admission limits. Work units bound loop iterations and compared bytes,
/// not elapsed time or machine instructions. See docs/architecture.md.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resource {
    Entries,
    PathBytes,
    TotalPathBytes,
    BlobBytes,
    LoadedBytes,
    Lines,
    TableCells,
    Work,
}

impl Resource {
    pub const fn limit(self) -> u64 {
        match self {
            Self::Entries => 4096,
            Self::PathBytes => 1024,
            Self::TotalPathBytes => 128 * 1024,
            Self::BlobBytes => 1024 * 1024,
            Self::LoadedBytes => 8 * 1024 * 1024,
            Self::Lines => 8192,
            Self::TableCells => 4 * 1024 * 1024,
            Self::Work => 512 * 1024 * 1024,
        }
    }

    pub(crate) fn check(self, required: u64) -> Result<(), ResourceLimit> {
        if required > self.limit() {
            Err(ResourceLimit {
                resource: self,
                required,
                limit: self.limit(),
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_limit_is_inclusive_and_work_is_cumulative() {
        for resource in [
            Resource::Entries,
            Resource::PathBytes,
            Resource::TotalPathBytes,
            Resource::BlobBytes,
            Resource::LoadedBytes,
            Resource::Lines,
            Resource::TableCells,
            Resource::Work,
        ] {
            assert!(resource.check(resource.limit()).is_ok());
            assert_eq!(
                resource.check(resource.limit() + 1).unwrap_err(),
                ResourceLimit {
                    resource,
                    required: resource.limit() + 1,
                    limit: resource.limit(),
                }
            );
        }
        let mut usage = Usage::default();
        usage.reserve(Resource::Work.limit() - 1).unwrap();
        usage.reserve(1).unwrap();
        assert_eq!(usage.reserve(1).unwrap_err().resource, Resource::Work);
        assert_eq!(usage.work_units, Resource::Work.limit());
    }
}

/// Deterministic refusal; no partial merge outcome is returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimit {
    pub resource: Resource,
    pub required: u64,
    pub limit: u64,
}

impl fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "merge resource {:?}: requires {}, limit {}",
            self.resource, self.required, self.limit
        )
    }
}
impl std::error::Error for ResourceLimit {}

#[derive(Debug, Eq, PartialEq)]
pub enum MergeError<E> {
    Load(E),
    Resource(ResourceLimit),
}

impl<E> From<ResourceLimit> for MergeError<E> {
    fn from(value: ResourceLimit) -> Self {
        Self::Resource(value)
    }
}
impl<E: fmt::Display> fmt::Display for MergeError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(error) => write!(f, "merge loader: {error}"),
            Self::Resource(error) => error.fmt(f),
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for MergeError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Load(error) => Some(error),
            Self::Resource(error) => Some(error),
        }
    }
}

/// Reserved work is a conservative admission cost, not actual executed work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub work_units: u64,
    pub loaded_bytes: u64,
}

impl Usage {
    pub(crate) fn reserve(&mut self, units: u64) -> Result<(), ResourceLimit> {
        let required = self.work_units.saturating_add(units);
        Resource::Work.check(required)?;
        self.work_units = required;
        Ok(())
    }

    pub(crate) fn loaded(&mut self, bytes: usize) -> Result<(), ResourceLimit> {
        Resource::BlobBytes.check(bytes as u64)?;
        let required = self.loaded_bytes.saturating_add(bytes as u64);
        Resource::LoadedBytes.check(required)?;
        self.reserve(4 * bytes as u64 + 32)?;
        self.loaded_bytes = required;
        Ok(())
    }
}
