use super::*;
use smallvec::{SmallVec, smallvec};

/// Keep the union of ordinary tree clips and transition endpoint clips.
/// Endpoints use their own coordinate space, even below hidden/offscreen parents.
pub(super) fn prune_scene_images(scene: &mut Scene) -> Result<(), String> {
    let mut endpoints = if scene.transitions.is_empty() {
        Vec::new()
    } else {
        vec![(false, None::<[i64; 2]>); scene.nodes.len()]
    };
    for transition in &scene.transitions {
        let first_only = transition.frame.phase == 0 && transition.custom.is_none();
        let last_only =
            transition.frame.phase >= transition.frame.effect.phases(transition.frame.size);
        for index in [
            (!last_only || first_only).then_some(transition.destination),
            (!first_only).then_some(transition.source),
        ]
        .into_iter()
        .flatten()
        {
            let Some((endpoint, extent)) = endpoints.get_mut(index) else {
                return Err("transition references a missing scene node".into());
            };
            *endpoint = true;
            if transition.with_children {
                let size = [
                    i64::from(transition.frame.size.width),
                    i64::from(transition.frame.size.height),
                ];
                *extent =
                    Some(extent.map_or(size, |old| [old[0].max(size[0]), old[1].max(size[1])]));
            }
        }
    }
    let mut clips: Vec<CaptureClip> = Vec::with_capacity(scene.nodes.len());
    for (index, node) in scene.nodes.iter_mut().enumerate() {
        let parent = match node.parent {
            Some(parent) if parent < index => Some(clips[parent]),
            Some(_) => return Err("scene parent must precede its children".into()),
            None => None,
        };
        let origin = parent.map_or((0, 0), |p| p.0);
        let origin = (
            origin.0 + i64::from(node.rectangle.left),
            origin.1 + i64::from(node.rectangle.top),
        );
        let own = [
            origin.0,
            origin.1,
            origin.0 + i64::from(node.rectangle.width),
            origin.1 + i64::from(node.rectangle.height),
        ];
        let mut clip = if !node.visible || node.opacity == 0 {
            None
        } else if let Some((_, clip)) = parent {
            clip.map(|p| {
                [
                    own[0].max(p[0]),
                    own[1].max(p[1]),
                    own[2].min(p[2]),
                    own[3].min(p[3]),
                ]
            })
        } else {
            Some(own)
        }
        .filter(|r| r[0] < r[2] && r[1] < r[3]);
        let (endpoint, extent) = endpoints.get(index).copied().unwrap_or_default();
        if !endpoint && clip.is_none() {
            node.image = None;
        }
        if let Some([width, height]) = extent.filter(|s| s[0] > 0 && s[1] > 0) {
            let area = [origin.0, origin.1, origin.0 + width, origin.1 + height];
            // A bounding union may keep extra pixels, but cannot discard an
            // input used by either ordinary composition or an endpoint.
            clip = Some(clip.map_or(area, |p| {
                [
                    p[0].min(area[0]),
                    p[1].min(area[1]),
                    p[2].max(area[2]),
                    p[3].max(area[3]),
                ]
            }));
        }
        clips.push((origin, clip));
    }
    Ok(())
}

pub(super) fn scene_images(scene: &Scene) -> Vec<ImageId> {
    scene
        .nodes
        .iter()
        .filter_map(|n| n.image.as_ref().map(|i| i.id))
        .chain(
            scene
                .transitions
                .iter()
                .filter_map(|t| t.rule.as_ref().map(|i| i.id)),
        )
        .collect()
}
pub(super) fn command_images(command: &Command) -> SmallVec<[ImageId; 2]> {
    use Command::*;
    match command {
        Create { .. } | LoadCompressed { .. } => smallvec![],
        Assign { source, .. } | SnapshotMain { source, .. } => smallvec![source.id],
        CreateProvince {
            operation: ProvinceOperation::Copy { source, .. },
            ..
        } => smallvec![source.id],
        CreateProvince { .. } => smallvec![],
        PreparedDraw(batch) => smallvec![batch.image().id],
        Sprites { image, source, .. }
        | Scanlines { image, source, .. }
        | Warp { image, source, .. }
        | Perspective { image, source, .. }
        | WrappedCopy { image, source, .. }
        | Transform { image, source, .. }
        | Operate { image, source, .. }
        | Copy { image, source, .. } => smallvec![image.id, source.id],
        EnableImage { source, .. }
        | AssignBitmap { source, .. }
        | BeginUpload { source, .. }
        | PrepareUpload { source, .. } => source.iter().map(|i| i.id).collect(),
        ComposeScene { scene, .. } => scene_images(scene).into(),
        PiledCopy { image, scene, .. } => {
            let mut ids = scene_images(scene);
            ids.push(image.id);
            ids.into()
        }
        Meshes { image, batch, .. } => {
            let mut ids = smallvec![image.id];
            ids.extend(batch.draws.iter().filter_map(|d| match &d.texture {
                krkr_protocol::mesh::Texture::Image(i) => Some(i.id),
                _ => None,
            }));
            ids
        }
        CopyPixels { image, .. }
        | Adjust { image, .. }
        | Text { image, .. }
        | Color { image, .. }
        | Independ { image, .. }
        | Upload { image, .. }
        | UploadScaled { image, .. }
        | UploadYuv { image, .. }
        | CopyYuv { image, .. }
        | PatchPixels { image, .. }
        | PatchRegion { image, .. }
        | Resize { image, .. }
        | Fill { image, .. }
        | Pixel { image, .. }
        | ReadImage { image, .. }
        | ReadRegion { image, .. }
        | ReadProvince { image, .. }
        | ReadHitPlane { image, .. } => smallvec![image.id],
    }
}
impl Graphics {
    pub(super) fn evict_cache(&self) -> bool {
        // Keep useful aliases when a colder allocation can actually yield
        // space. Dropping a live alias only forces the next load to decode again.
        self.cache.evict_where(|entry| {
            Arc::strong_count(&entry.image.lifetime) == 1
                && self
                    .images
                    .get(&entry.image.id)
                    .is_some_and(|image| image.reclaimable_bytes() != 0)
        }) || self.cache.evict()
    }
    /// Reclaim before executing a draw: the driver may need physical backing
    /// long before our payload-only byte budget fills. Count actual released
    /// allocations, not cache entries which can alias live scene images.
    pub(super) fn reclaim_physical(
        &mut self,
        bytes: usize,
        protected: &[ImageId],
    ) -> Result<(), String> {
        let _profile = krkr_protocol::profile::span("graphics.reclaim_physical");
        let Some(free) = (self.physical_free)() else {
            return Ok(());
        };
        let required = bytes.saturating_add(crate::memory::GRAPHICS_HEADROOM_BYTES);
        if free >= required {
            self.pressure = None;
            return Ok(());
        }
        let usage = || {
            self.gpu
                .resident
                .used()
                .saturating_add(self.gpu.scratch.used())
        };
        let anticipated = usage().saturating_add(bytes);
        if self
            .pressure
            .is_some_and(|(previous_free, previous_usage)| {
                free >= previous_free && anticipated <= previous_usage
            })
        {
            // Kernel totals exclude space retained inside the driver heaps.
            // Unchanged pressure must not flush every frame or repeatedly
            // evict caches which cannot release another physical block.
            return Ok(());
        }
        let target = self
            .gpu
            .resident
            .available()
            .saturating_add(required - free)
            .min(self.gpu.resident.limit());
        self.reap();
        self.gpu
            .collect_under_pressure()
            .map_err(|e| e.to_string())?;
        let recovered = || (self.physical_free)().is_some_and(|free| free >= required);
        if !recovered() {
            while self.gpu.capacity_after_collect(&self.gpu.resident) < target && self.evict_cache()
            {
                self.reap();
            }
            self.gpu.collect().map_err(|e| e.to_string())?;
            if !(self.physical_free)().is_some_and(|free| free >= required) {
                self.make_room(target, protected)?;
            }
        }
        self.pressure = Some((
            (self.physical_free)().unwrap_or(free),
            self.gpu
                .resident
                .used()
                .saturating_add(self.gpu.scratch.used())
                .saturating_add(bytes),
        ));
        Ok(())
    }
    pub(super) fn ensure_images(&mut self, needed: &[ImageId]) -> Result<(), String> {
        self.serial = self.serial.wrapping_add(1);
        for id in needed {
            self.touched.insert(*id, self.serial);
        }
        for id in needed {
            let Some(bytes) = self.spilled.get(id).map(|s| s.gpu_bytes()) else {
                continue;
            };
            self.reclaim_physical(bytes, needed)?;
            self.make_room(bytes, needed)?;
            if bytes > self.gpu.resident.available() {
                // Scene restoration needs the same last-resort compaction as
                // drawing commands, before allocating any part of the image.
                self.gpu
                    .reclaim_canvas_borders(self.images.values_mut(), bytes)
                    .map_err(|e| e.to_string())?;
            }
            self.restore_parked(*id)?;
        }
        Ok(())
    }
    fn restore_parked(&mut self, id: ImageId) -> Result<(), String> {
        let saved = self.spilled.remove(&id).unwrap();
        match self.gpu.restore_canvas(&saved) {
            Ok(image) => {
                let aliases: Vec<_> = self
                    .spilled
                    .iter()
                    .filter_map(|(key, data)| std::rc::Rc::ptr_eq(data, &saved).then_some(*key))
                    .collect();
                for key in aliases {
                    self.spilled.remove(&key);
                    self.images.insert(key, image.shared());
                }
                self.images.insert(id, image);
            }
            Err(error) => {
                self.spilled.insert(id, saved);
                krkr_protocol::profile::marker("graphics.restore_failure", || {
                    let mut inventory: Vec<_> = self.images.iter().collect();
                    inventory.sort_by_key(|(_, image)| std::cmp::Reverse(image.resident_bytes()));
                    format!(
                        "id={id:?} error={error}; staging={} scratch={}; images={inventory:?}",
                        self.gpu.staging.used(),
                        self.gpu.scratch.used(),
                    )
                });
                return Err(format!("restoring canvas {id:?}: {error}"));
            }
        }
        Ok(())
    }
    /// Parked canvases must yield CPU space to the next decode. Restore only
    /// what fits beside the pending GPU allocation; never spill another canvas
    /// while trying to free staging, which would just exchange the same bytes.
    pub(super) fn make_staging_room(
        &mut self,
        bytes: usize,
        gpu_pending: usize,
    ) -> Result<(), String> {
        if self.gpu.staging.available() >= bytes {
            return Ok(());
        }
        self.maintain()?;
        self.gpu.collect().map_err(|e| e.to_string())?;
        let mut candidates: Vec<_> = self
            .spilled
            .iter()
            .map(|(id, saved)| (*id, saved.staging_bytes(), saved.gpu_bytes()))
            .collect();
        // Recover the most CPU bytes per GPU byte first.
        candidates.sort_by(|a, b| {
            (u128::from(b.1 as u64) * a.2 as u128).cmp(&(u128::from(a.1 as u64) * b.2 as u128))
        });
        for (id, _, gpu_bytes) in candidates {
            if self.gpu.staging.available() >= bytes {
                break;
            }
            if !self.spilled.contains_key(&id) {
                continue;
            }
            while gpu_bytes.saturating_add(gpu_pending)
                > self.gpu.capacity_after_collect(&self.gpu.resident)
                && self.evict_cache()
            {
                self.reap();
            }
            if gpu_bytes.saturating_add(gpu_pending) > self.gpu.resident.available() {
                self.gpu.collect().map_err(|e| e.to_string())?;
            }
            if self.spilled.contains_key(&id)
                && gpu_bytes.saturating_add(gpu_pending) <= self.gpu.resident.available()
            {
                self.restore_parked(id)?;
            }
        }
        Ok(())
    }
    pub(super) fn make_room(&mut self, bytes: usize, protected: &[ImageId]) -> Result<(), String> {
        let _profile = krkr_protocol::profile::span_detail("graphics.make_room", || {
            format!("requested={bytes}")
        });
        if bytes <= self.gpu.resident.available() {
            return Ok(());
        }
        self.maintain()?;
        self.gpu.collect().map_err(|e| e.to_string())?;
        // Restoring an evicted canvas reaches this path before command
        // admission. Release optional image-cache owners here too, otherwise
        // cold assets can force live canvases into CPU staging indefinitely.
        let mut evicted = false;
        while bytes > self.gpu.capacity_after_collect(&self.gpu.resident) && self.evict_cache() {
            evicted = true;
            self.reap();
        }
        if evicted {
            self.gpu.collect().map_err(|e| e.to_string())?;
        }
        if bytes > self.gpu.resident.available() {
            self.compact_canvases(bytes)?;
        }
        if bytes <= self.gpu.resident.available() {
            return Ok(());
        }
        let mut seen = std::collections::HashSet::new();
        let mut candidates = Vec::new();
        for (id, image) in &self.images {
            if !seen.insert(*id) {
                continue;
            }
            let group: Vec<_> = self
                .images
                .iter()
                .filter_map(|(key, other)| image.same_canvas_storage(other).then_some(*key))
                .collect();
            seen.extend(group.iter().copied());
            if group.is_empty()
                || group
                    .iter()
                    .any(|key| protected.contains(key) || self.uploads.contains_key(key))
            {
                continue;
            }
            let images: Vec<_> = group.iter().map(|key| &self.images[key]).collect();
            let age = group
                .iter()
                .map(|key| self.touched.get(key).copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
            if self.gpu.spill_group_bytes(&images) >= 256 * 1024 {
                let reclaimed = self.gpu.spill_group_reclaim_bytes(&images);
                candidates.push((vec![group], age, reclaimed));
            }
        }
        // Prefer the existing single-plane LRU order. Combining independently
        // reclaimable planes parks more canvases than the request needs.
        let (mut candidates, mut independent): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|(_, _, reclaimed)| *reclaimed < 256 * 1024);
        // A full copy can own a different plane while retaining the original
        // texture. Park connected cold planes together, otherwise every view
        // appears pinned by its neighbours and none can yield any storage.
        let mut index = 0;
        while index < candidates.len() {
            let mut other = index + 1;
            while other < candidates.len() {
                let shared = candidates[index].0.iter().any(|a| {
                    candidates[other]
                        .0
                        .iter()
                        .any(|b| self.images[&a[0]].shares_main_storage(&self.images[&b[0]]))
                });
                if shared {
                    let (groups, age, _) = candidates.swap_remove(other);
                    candidates[index].0.extend(groups);
                    candidates[index].1 = candidates[index].1.max(age);
                    // The added groups can connect an earlier unmerged peer.
                    other = index + 1;
                } else {
                    other += 1;
                }
            }
            index += 1;
        }
        for (groups, _, reclaimed) in &mut candidates {
            let images: Vec<Vec<_>> = groups
                .iter()
                .map(|group| group.iter().map(|id| &self.images[id]).collect())
                .collect();
            let views: Vec<_> = images.iter().map(Vec::as_slice).collect();
            *reclaimed = self.gpu.spill_batch_reclaim_bytes(&views);
        }
        candidates.retain(|(_, _, reclaimed)| *reclaimed >= 256 * 1024);
        candidates.sort_by_key(|(_, age, bytes)| (*age, std::cmp::Reverse(*bytes)));
        independent.sort_by_key(|(_, age, bytes)| (*age, std::cmp::Reverse(*bytes)));
        independent.extend(candidates);
        for (groups, _, _) in independent {
            if bytes <= self.gpu.resident.available() {
                break;
            }
            // Keep the decompressor and one upload strip usable after eviction.
            let Ok(_workspace) = self.gpu.staging.reserve(256 * 1024) else {
                break;
            };
            let mut saved = Vec::with_capacity(groups.len());
            for group in &groups {
                let images: Vec<_> = group.iter().map(|key| &self.images[key]).collect();
                match self.gpu.spill_canvas_group(&images) {
                    Ok(Some(packed)) => saved.push(std::rc::Rc::new(packed)),
                    Ok(None) | Err(krkr_render::Error::Budget(_)) => break,
                    Err(error) => return Err(error.to_string()),
                }
            }
            if saved.len() != groups.len() {
                continue;
            }
            for (group, packed) in groups.into_iter().zip(saved) {
                for id in group {
                    self.spilled.insert(id, packed.clone());
                    self.images.remove(&id);
                }
            }
            self.gpu.collect().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
