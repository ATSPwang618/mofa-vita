//! Optional distribution language policy. Save/script IDs remain the original IDs.
use anyhow::{Context, Result, ensure};
use std::{collections::HashMap, path::Path};

pub struct Profile {
    ids: Vec<usize>,
    pub aliases: HashMap<String, Vec<String>>,
}

impl Profile {
    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = root.join("language-profile.txt");
        if !path.exists() { return Ok(None); }
        let text = std::fs::read_to_string(path).context("reading language profile")?;
        let mut lines = text.lines();
        ensure!(lines.next() == Some("knife-languages-v1"), "unknown language profile");
        let ids = lines.map(|line| {
            line.split('\t').next().context("missing language ID")?.parse::<usize>().context("invalid language ID")
        }).collect::<Result<Vec<_>>>()?;
        ensure!(ids == [1, 2], "this compact profile requires English 1 and Chengkong Chinese 2");
        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for line in std::fs::read_to_string(root.join("ui-aliases.tsv")).context("reading UI aliases")?.lines() {
            let (base, name) = line.split_once('\t').context("invalid UI alias")?;
            aliases.entry(base.to_ascii_uppercase()).or_default().push(name.to_string());
        }
        Ok(Some(Self { ids, aliases }))
    }

    pub fn resolve(&self, word: i16, system: i16) -> usize {
        let id = if word < 0 || word > 3 { system } else { word };
        if id == 2 || id == 3 { 2 } else { 1 }
    }

    pub fn slot(&self, id: usize) -> usize {
        self.ids.iter().position(|&value| value == id).unwrap_or(0)
    }

    pub fn cycle(&self, id: usize, forward: bool) -> i16 {
        let slot = self.slot(id);
        self.ids[(slot + if forward { 1 } else { self.ids.len() - 1 }) % self.ids.len()] as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile { Profile { ids: vec![1, 2], aliases: HashMap::new() } }
    #[test]
    fn legacy_save_ids_are_mapped_without_renumbering_chengkong() {
        let p = profile();
        assert_eq!((p.resolve(1, 0), p.resolve(2, 0)), (1, 2));
        assert_eq!((p.resolve(0, 0), p.resolve(3, 0)), (1, 2));
        assert_eq!((p.resolve(-1, 3), p.resolve(-1, 0)), (2, 1));
        assert_eq!((p.slot(1), p.slot(2)), (0, 1));
    }
    #[test]
    fn language_arrows_visit_only_english_and_chengkong_in_both_directions() {
        let p = profile();
        for forward in [false, true] {
            assert_eq!(p.cycle(1, forward), 2);
            assert_eq!(p.cycle(2, forward), 1);
        }
    }
}
