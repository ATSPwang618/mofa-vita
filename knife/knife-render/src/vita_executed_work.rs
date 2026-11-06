//! Fixed draw metadata is prepared while recording. Only successful native
//! submissions update these cumulative CPU counters. Area is a clipped triangle
//! estimate; none of these counters measure hardware fragments or GPU time.
use super::{EffectBlend, EffectShader, Vertex};
use std::{fmt::Write, sync::Mutex};

const TABLE: usize = 128;
const TOP: usize = 8;
const OTHER_ROLE: u8 = 4;
const MATERIAL_FEATURES: usize = 6;
// ordinary, snapshot PS, generic fetch PS, specialized PS, two-op chain,
// direct op18, weight-uniform blur, original blur.
const SELECTIONS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DrawWork {
    pub ops: [u8; 2], pub op_count: u8, pub flags: u8, pub role: u8,
    pub program: u8, pub selection: u8, pub target_role: u8,
    pub target_size: [u32; 2], pub source_draws: u32, pub pixels: u64,
}
impl DrawWork {
    pub const fn pending() -> Self {
        Self { ops: [0;2], op_count: 0, flags: 0, role: OTHER_ROLE, program: u8::MAX,
            selection: 0, target_role: 0, target_size: [0;2], source_draws: 0, pixels: 0 }
    }
    pub fn prepare(shader: EffectShader, blend: EffectBlend, params: [f32;8], vertices: &[Vertex],
        target_size: [u32;2], target_role: u32) -> Self {
        let id = shader as u32;
        let direct = params[7] == -18.0;
        let ps = matches!(id,14|17..=21|23|25|27|29..=31|48..=52|54..=56|64..=87) || direct;
        let op = |v: f32| if v.is_finite() && v.fract() == 0.0 && (1.0..=24.0).contains(&v) { v as u8 } else { 0 };
        let ops = if ps { [op(params[0]), if id == 27 { op(params[1]) } else { 0 }] } else { [0;2] };
        let op_count = u8::from(ops[0] != 0) + u8::from(ops[1] != 0);
        let mut flags = 0u8;
        // Chain UV/tint and RimText bounds use haze slots as transport, but are
        // not wave effects. Classify actual material inputs rather than storage.
        if matches!(id,0..=3|6|7|9|14|16..=21|23|25|29..=31|48..=52|54..=56|64..=87)
            && vertices.iter().any(|v|v.haze.iter().flatten().any(|x|*x!=0.0)) { flags |= 1; }
        if vertices.iter().any(|v|v.soft[2][0]>0.0) { flags |= 4; }
        if ps && !direct && id != 27 {
            if params[4]>=0.0 { flags |= 2; }
            if params[7]==1.0 { flags |= 8; }
            if params[1]==1.0 { flags |= 16; }
            if params[6]==1.0 { flags |= 32; }
        } else {
            if id==9 || (id==8 && params[2]>0.5) { flags |= 2; }
            if id==15 || id==16 { flags |= 8; }
            if id==3 { flags |= 16; }
            if id==1 { flags |= 32; }
        }
        let role = if matches!(id,10|53) {3} else if matches!(id,15|19) || (ps && params[7]==1.0) {2}
            else if target_role==2 {0} else if target_role==3 || blend==EffectBlend::MaskAlpha {1} else {OTHER_ROLE};
        let selection = if direct {5} else if id==27 {4} else if id==53 {6} else if id==10 {7}
            else if ps && id==14 {1} else if ps && id==17 {2} else if ps {3} else {0};
        let program = if id == 27 && ops == [7,7] {57} else {id.min(254) as u8};
        Self { ops, op_count, flags, role, program, selection,
            target_role: target_role.min(15) as u8, target_size,
            source_draws: u32::from(op_count.max(1)), pixels: area(vertices,target_size) }
    }
    pub fn merge(&mut self, rhs: Self) {
        debug_assert_eq!((self.ops,self.program,self.target_size,self.target_role),
            (rhs.ops,rhs.program,rhs.target_size,rhs.target_role));
        self.flags |= rhs.flags;
        self.source_draws = self.source_draws.saturating_add(rhs.source_draws);
        self.pixels = self.pixels.saturating_add(rhs.pixels);
    }
    pub fn refresh_geometry(&mut self, vertices: &[Vertex]) { self.pixels = area(vertices,self.target_size); }
    fn key(self) -> u32 {
        u32::from(self.ops[0]) | u32::from(self.ops[1])<<5 | u32::from(self.flags)<<10
            | u32::from(self.program)<<16 | u32::from(self.target_role)<<24
    }
    fn fallback(self) -> bool { matches!(self.selection,1|2|7) }
    fn specialized(self) -> bool { matches!(self.selection,3..=6) }
}

/// Clip every submitted triangle independently to the physical target. Sum
/// overlaps separately to preserve duplicated work in merged draw geometry.
fn area(vertices: &[Vertex], [w,h]: [u32;2]) -> u64 {
    let mut total=0.0f64;
    for triangle in vertices.chunks_exact(3) {
        let mut a=[[0.0f64;2]; 8]; let mut n=3;
        for (dst,src) in a.iter_mut().zip(triangle) { *dst=[f64::from(src.pos[0]),f64::from(src.pos[1])]; }
        if a[..3].iter().flatten().any(|v|!v.is_finite()) { continue; }
        for (axis,edge,greater) in [(0,-1.0,true),(0,1.0,false),(1,-1.0,true),(1,1.0,false)] {
            if n==0 { break; }
            let mut b=[[0.0;2];8]; let mut count=0;
            let inside=|p:[f64;2]| if greater {p[axis]>=edge} else {p[axis]<=edge};
            let mut prev=a[n-1]; let mut pin=inside(prev);
            for &cur in &a[..n] {
                let cin=inside(cur);
                if pin!=cin {
                    let t=(edge-prev[axis])/(cur[axis]-prev[axis]);
                    b[count]=[prev[0]+t*(cur[0]-prev[0]),prev[1]+t*(cur[1]-prev[1])]; count+=1;
                }
                if cin { b[count]=cur; count+=1; }
                prev=cur; pin=cin;
            }
            a=b; n=count;
        }
        if n<3 { continue; }
        let mut twice=0.0;
        for i in 0..n { let j=(i+1)%n; twice+=a[i][0]*a[j][1]-a[j][0]*a[i][1]; }
        total+=twice.abs()*0.5*f64::from(w)*f64::from(h)*0.25;
    }
    total.ceil().max(0.0).min(u64::MAX as f64) as u64
}

#[derive(Clone, Copy, Default)]
struct Counts { calls:u64, sources:u64, pixels:u64, specialized:u64, fallback:u64 }
impl Counts {
    fn add(&mut self, rhs: Self) {
        self.calls=self.calls.saturating_add(rhs.calls); self.sources=self.sources.saturating_add(rhs.sources);
        self.pixels=self.pixels.saturating_add(rhs.pixels);
        self.specialized=self.specialized.saturating_add(rhs.specialized); self.fallback=self.fallback.saturating_add(rhs.fallback);
    }
}
#[derive(Clone, Copy, Default)]
struct Material { key:u32, used:bool, counts:Counts }
struct Stats {
    total:Counts, ops:[Counts;25], roles:[Counts;4], materials:[Material;TABLE],
    programs:[u64;128], selections:[u64;SELECTIONS], feature_counts:[Counts;MATERIAL_FEATURES], overflow:Counts,
}
impl Stats {
    const fn new() -> Self {
        const ZERO:Counts=Counts{calls:0,sources:0,pixels:0,specialized:0,fallback:0};
        Self {total:ZERO,ops:[ZERO;25],roles:[ZERO;4],materials:[Material{key:0,used:false,counts:ZERO};TABLE],
            programs:[0;128],selections:[0;SELECTIONS],feature_counts:[ZERO;MATERIAL_FEATURES],overflow:ZERO}
    }
    fn record(&mut self, work: DrawWork) {
        if work.program==u8::MAX { return; }
        let counts=Counts{calls:1,sources:u64::from(work.source_draws),pixels:work.pixels,
            specialized:u64::from(work.specialized()),fallback:u64::from(work.fallback())};
        self.total.add(counts);
        let mut per_op=counts;
        per_op.sources/=u64::from(work.op_count.max(1));
        if work.op_count==0 { self.ops[0].add(counts); }
        else { for &op in &work.ops[..usize::from(work.op_count)] { self.ops[usize::from(op)].add(per_op); } }
        if usize::from(work.program)<self.programs.len() {
            self.programs[usize::from(work.program)]=self.programs[usize::from(work.program)].saturating_add(1);
        }
        self.selections[usize::from(work.selection)]=self.selections[usize::from(work.selection)].saturating_add(1);
        if usize::from(work.role)<self.roles.len() {self.roles[usize::from(work.role)].add(counts);}
        // Feature bits are deliberately reported independently from the
        // collision-safe material key, so the bounded JSON remains useful
        // without requiring callers to decode key packing.  Bits are Haze,
        // rule, soft clip, group mask, Fill and NoAlpha respectively.
        for bit in 0..MATERIAL_FEATURES {
            if work.flags & (1u8 << bit) != 0 { self.feature_counts[bit].add(counts); }
        }
        let key=work.key(); let start=key.wrapping_mul(2654435761) as usize%TABLE;
        for offset in 0..TABLE {
            let row=&mut self.materials[(start+offset)%TABLE];
            if !row.used || row.key==key {row.used=true;row.key=key;row.counts.add(counts);return;}
        }
        self.overflow.add(counts);
    }
    fn json(&self)->String {
        let mut out=String::with_capacity(4096);
        let _=write!(out,"{{\"schema\":3,\"scope\":\"successful-native-draws-cumulative\",\"coverage_unit\":\"estimated_covered_pixels\",\"totals\":[{},{},{}],\"ops\":[",self.total.calls,self.total.sources,self.total.pixels);
        let mut first=true;
        for (op,c) in self.ops.iter().enumerate().skip(1).filter(|(_,c)|c.calls>0) {
            if !first{out.push(',');}first=false;
            let _=write!(out,"[{op},{},{},{},{},{}]",c.calls,c.sources,c.pixels,c.specialized,c.fallback);
        }
        out.push_str("],\"roles_pixel_mask_composite_blur\":[");
        for (i,c) in self.roles.iter().enumerate(){if i>0{out.push(',');}let _=write!(out,"[{},{}]",c.calls,c.pixels);}
        out.push_str("],\"material_top\":[");
        let mut top=[None;TOP];
        for row in self.materials.iter().filter(|r|r.used) {
            for i in 0..TOP {
                if top[i].is_none_or(|r:Material|r.counts.pixels<row.counts.pixels) {
                    for j in (i+1..TOP).rev(){top[j]=top[j-1];}top[i]=Some(*row);break;
                }
            }
        }
        for (i,row) in top.iter().flatten().enumerate(){if i>0{out.push(',');}let _=write!(out,"[{},{},{}]",row.key,row.counts.calls,row.counts.pixels);}
        out.push_str("],\"selection\":[");
        for (i,count) in self.selections.iter().enumerate(){if i>0{out.push(',');}let _=write!(out,"{count}");}
        // Reserve the fixed tail before admitting variable program rows. Hot
        // plain material programs are retained first even in saturated u64
        // synthetic output; omitted rows are explicit rather than silent loss.
        out.push_str("],\"program_counts\":[");
        let mut emitted=0usize;let mut omitted=0usize;
        let mut selected=[false;128];
        for id in [48usize,57,30,31,21,29,54,55,56].into_iter().chain(64usize..88).chain([49usize,50,51,52,53]).chain(0..128) {
            if selected[id]{continue;}selected[id]=true;
            let count=self.programs[id];if count==0{continue;}
            let mut row=String::with_capacity(26);let _=write!(row,"[{id},{count}]");
            if out.len()+usize::from(emitted!=0)+row.len()+128<=4096 {
                if emitted!=0{out.push(',');}out.push_str(&row);emitted+=1;
            } else {omitted+=1;}
        }
        // Keep feature detail after the prioritized program rows.  This lets
        // a saturated diagnostic still retain the hot plain-program choices
        // while preserving the fixed 4 KiB transport bound.
        let mut feature_tail=String::from("],\"program_omitted\":");
        let _=write!(feature_tail,"{omitted},\"material_features\":[");
        let mut first_feature=true;
        for (bit,c) in self.feature_counts.iter().enumerate().filter(|(_,c)| c.calls>0) {
            if !first_feature { feature_tail.push(','); }
            first_feature=false;
            let _=write!(feature_tail,"[{bit},{},{}]",c.calls,c.pixels);
        }
        let _=write!(feature_tail,"] ,\"material_feature_bits\":\"haze,rule,soft,group_mask,fill,noalpha\",\"overflow\":[{},{}]}}",self.overflow.calls,self.overflow.pixels);
        // Keep the complete material top-eight table whenever the fixed
        // 4 KiB envelope permits it.  Saturated synthetic counters (or a
        // future larger counter schema) may otherwise crowd out the program
        // selection rows; in that case the top table is explicitly omitted
        // while program and feature summaries remain available.
        if out.len().saturating_add(feature_tail.len()) > 4096 {
            if let (Some(material), Some(selection)) =
                (out.find("],\"material_top\":["), out.find("],\"selection\":["))
            {
                let suffix = out[selection..].to_owned();
                out.truncate(material);
                out.push_str("],\"material_top\":[]");
                out.push_str(&suffix);
                feature_tail = feature_tail.replace("],\"overflow\"", "],\"material_top_omitted\":true,\"overflow\"");
            }
        }
        out.push_str(&feature_tail);
        debug_assert!(out.len()<=4096);
        out
    }
}
static STATS:Mutex<Stats>=Mutex::new(Stats::new());
pub(super) fn snapshot()->(u64,u64){let stats=STATS.lock().unwrap_or_else(|e|e.into_inner());(stats.total.calls,stats.total.pixels)}
pub(super) fn record_success(work:DrawWork){
    if !knife_perf::enabled(){return;}
    for &op in &work.ops[..usize::from(work.op_count)] {knife_perf::record_ps(u32::from(op));}
    STATS.lock().unwrap_or_else(|e|e.into_inner()).record(work);
}
pub(super) fn json()->String{STATS.lock().unwrap_or_else(|e|e.into_inner()).json()}

#[cfg(test)]
mod tests {
    use super::*;
    fn work(shader:EffectShader,params:[f32;8])->DrawWork{DrawWork::prepare(shader,EffectBlend::Opaque,params,&super::super::fullscreen_verts([1.;4]),[100,50],0)}
    #[test]fn real_target_clipping_and_duplicate_work_are_preserved(){
        let full=super::super::fullscreen_verts([1.;4]);
        assert_eq!(area(&full,[960,540]),518400);
        assert_eq!(area(&[full.as_slice(),full.as_slice()].concat(),[100,50]),10000);
        let mut half=full;for v in &mut half{v.pos[0]=(v.pos[0]+1.)*0.5;}
        assert_eq!(area(&half,[100,50]),2500);
        let mut outside=full;for v in &mut outside{v.pos[0]+=2.;}assert_eq!(area(&outside,[100,50]),0);
    }
    #[test]fn chain_tracks_two_ops_without_doubling_native_draw_totals(){
        let op7=work(EffectShader::PsChain2,[7.,7.,100.,50.,-1.,0.,0.,0.]);
        assert_eq!((op7.program,op7.ops,op7.source_draws,op7.selection),(57,[7,7],2,4));
        let w=work(EffectShader::PsChain2,[11.,13.,100.,50.,-1.,0.,0.,0.]);
        assert_eq!((w.ops,w.op_count,w.source_draws,w.flags),([11,13],2,2,0));
        let mut stats=Stats::new();stats.record(w);
        assert_eq!((stats.total.calls,stats.total.sources,stats.total.pixels),(1,2,5000));
        assert_eq!((stats.ops[11].calls,stats.ops[13].calls),(1,1));
    }
    #[test]fn material_and_new_programs_are_prepared_before_execution(){
        let op7=work(EffectShader::PsPlain7,[7.,0.,100.,50.,-1.,0.,0.,0.]);
        assert_eq!((op7.program,op7.ops,op7.selection),(48,[7,0],3));
        for (shader,op) in [(EffectShader::PsFetch1,1),(EffectShader::PsFetch6,6),(EffectShader::PsFull1,1),(EffectShader::PsFull6,6),(EffectShader::PsFull13,13),(EffectShader::PsHalf1,1),(EffectShader::PsHalf6,6),(EffectShader::PsHalf7,7),(EffectShader::PsHalf13,13)]{
            let w=work(shader,[op as f32,1.,100.,50.,-1.,0.,1.,0.]);
            assert_eq!((w.ops[0],w.flags,w.selection),(op,16|32,3));
        }
        let direct=work(EffectShader::NoAlpha,[18.,0.,0.,0.,0.,0.,0.,-18.]);
        assert_eq!((direct.ops[0],direct.flags,direct.selection),(18,32,5));
        assert_eq!(work(EffectShader::BlurWeights,[0.;8]).role,3);
    }
    #[test]fn merges_accumulate_sources_and_geometry_and_refresh_restored_cut(){
        let mut a=work(EffectShader::PsFetch1,[1.,0.,100.,50.,-1.,0.,0.,0.]);let b=a;
        a.merge(b);assert_eq!((a.source_draws,a.pixels),(2,10000));
        a.refresh_geometry(&super::super::fullscreen_verts([1.;4]));assert_eq!((a.source_draws,a.pixels),(2,5000));
    }
    #[test]fn feature_bits_are_reported_separately_from_material_keys(){
        let mut stats=Stats::new();
        let mut draw=work(EffectShader::PsFetch1,[1.,1.,100.,50.,0.25,0.,1.,1.]);
        draw.flags=0b11_1111;
        stats.record(draw);
        let json=stats.json();
        assert!(json.contains("\"material_feature_bits\":\"haze,rule,soft,group_mask,fill,noalpha\""));
        assert!(json.contains("\"material_features\":[[0,1,5000],[1,1,5000],[2,1,5000],[3,1,5000],[4,1,5000],[5,1,5000]]"));
    }
    #[test]fn table_storage_and_worst_case_json_are_bounded(){
        assert!(std::mem::size_of::<Stats>()<128*1024);assert!(std::mem::size_of::<DrawWork>()<=48);
        let max=Counts{calls:u64::MAX,sources:u64::MAX,pixels:u64::MAX,specialized:u64::MAX,fallback:u64::MAX};
        let mut stats=Stats::new();stats.total=max;stats.ops=[max;25];stats.roles=[max;4];stats.selections=[u64::MAX;SELECTIONS];stats.programs=[u64::MAX;128];stats.feature_counts=[max;MATERIAL_FEATURES];stats.overflow=max;
        for (i,row) in stats.materials.iter_mut().enumerate(){*row=Material{key:u32::MAX-i as u32,used:true,counts:max};}
        let json=stats.json();assert!(json.len()<=4096,"{}",json.len());assert!(json.ends_with('}'));
        assert!(json.contains("[30,18446744073709551615]"));assert!(json.contains("[31,18446744073709551615]"));
        assert!(!json.contains("\"program_omitted\":0,"));
    }
}
