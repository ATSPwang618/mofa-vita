//! Profile-bound metadata only: registration never retains/decompresses UI pixels.
use knife_render::IndexedGxtMetadata;
pub struct Member {
    pub name: &'static str, pub offset: usize, pub len: usize,
    pub logical: [u32;2], pub physical: [u32;2], pub encoding: u32,
    pub gxt: Option<IndexedGxtMetadata>,
}
pub struct Atlas {
    pub index:usize,pub entry:&'static str,pub offset:u64,
    pub stored_sectors:u16,pub unpacked_sectors:u16,pub decoded_bytes:usize,
    pub members:&'static [Member],
}
include!("../data/vita_atlas_index_540p.rs");
pub fn find(manifest:&str,index:usize,entry:&knife_core::format::archive::ArchiveEntry)->Option<&'static Atlas>{
    if manifest!=MANIFEST{return None;}
    ATLASES.iter().find(|a|a.index==index && a.entry==entry.name && a.offset==entry.offset
        && a.stored_sectors==entry.stored_sectors && a.unpacked_sectors==entry.unpacked_sectors)
}
