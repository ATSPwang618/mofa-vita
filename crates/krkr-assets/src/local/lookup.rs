//! Short-lived directory snapshots for optional sibling-name probes.
use super::*;

#[derive(Default)]
pub(crate) struct Lookup {
    directories: Vec<(PathBuf, Option<Vec<Vec<u16>>>)>,
    names: usize,
    units: usize,
}

impl Lookup {
    pub fn metadata(&mut self, path: &Path) -> Result<(PathBuf, fs::Metadata)> {
        let parent = path.parent();
        let leaf = path
            .file_name()
            .map(|leaf| units(Path::new(leaf)))
            .transpose()?;
        let folded = leaf.as_deref().map(name::fold);
        let cached =
            parent.and_then(|parent| self.directories.iter().find(|(path, _)| path == parent));
        if let (Some(leaf), Some((_, Some(names)))) = (folded.as_ref(), cached)
            && names.binary_search(leaf).is_err()
        {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
        }
        // Check live exact metadata first. On a miss, snapshot the parent
        // before case-fold fallback; resolving the missing leaf first would
        // enumerate that same directory twice.
        let result = match super::stat(path) {
            Ok(metadata) => return Ok((path.to_owned(), metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => error,
            Err(error) => return Err(error.into()),
        };
        if let Some(parent) = parent
            && cached.is_none()
            && self.directories.len() < 8
        {
            let names = snapshot(parent, 1024 - self.names, 8192 - self.units);
            if let Some(names) = &names {
                self.names += names.len();
                self.units += names.iter().map(Vec::len).sum::<usize>();
            }
            // Remember rejected snapshots too, so each alternate extension
            // does not enumerate the same oversized directory again.
            self.directories.push((parent.to_owned(), names));
        }
        if let (Some(parent), Some(leaf)) = (parent, folded.as_ref())
            && let Some((_, Some(names))) = self.directories.iter().find(|(path, _)| path == parent)
            && names.binary_search(leaf).is_err()
        {
            return Err(result.into());
        }
        super::metadata(path)
    }
}

fn snapshot(parent: &Path, max_names: usize, max_units: usize) -> Option<Vec<Vec<u16>>> {
    #[cfg(target_os = "vita")]
    let timer = krkr_protocol::diagnostics::Timer::start();
    let result = snapshot_inner(parent, max_names, max_units);
    #[cfg(target_os = "vita")]
    timer.report(|| format!("stage=local-directory path={}", parent.display()));
    result
}
fn snapshot_inner(parent: &Path, max_names: usize, max_units: usize) -> Option<Vec<Vec<u16>>> {
    let entries = match super::read_dir(&super::resolve(parent).ok()?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut names = Vec::new();
    let mut units = 0;
    for entry in entries {
        let leaf = super::units(Path::new(&entry.ok()?.file_name())).ok()?;
        units += leaf.len();
        // Bound temporary storage independently of a game's directory size.
        // Large directories retain ordinary lookup without a partial snapshot.
        if names.len() == max_names || units > max_units {
            return None;
        }
        names.push(name::fold(&leaf));
    }
    names.sort_unstable();
    Some(names)
}
