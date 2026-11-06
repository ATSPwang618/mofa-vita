use super::*;

struct Profiling(bool);
impl Profiling {
    fn on() -> Self { let old = knife_perf::enabled(); knife_perf::set_enabled(true); Self(old) }
}
impl Drop for Profiling { fn drop(&mut self) { knife_perf::set_enabled(self.0); } }
struct Options(u64);
impl Options {fn set(mask:u64)->Self{let old=knife_perf::optimizations::mask();knife_perf::optimizations::set(mask);Self(old)}}
impl Drop for Options {fn drop(&mut self){knife_perf::optimizations::set(self.0);}}

#[test]
fn failed_and_abandoned_native_draws_do_not_increment_executed_ps_work() {
    let _options=Options::set(knife_perf::optimizations::PS_FETCH | knife_perf::optimizations::PS_OP7);
    let gxm = Gxm::new().unwrap(); let control = gxm.control.clone();
    control.borrow_mut().shader_capabilities = 1u64 << 29;
    let mut renderer = Renderer::new_with_gxm(gxm).unwrap();
    let target = renderer.create_target(8,8,"work failure target");
    let quad = Quad { blend: Blend::Ps(PsOp::DodgeAlpha),
        ..Quad::rect(renderer.white.clone(),[0.,0.,8.,8.],[1.;4]) };
    let _profile = Profiling::on();
    let metric = knife_perf::get(Metric::PsOp7); let before = executed_work_snapshot();
    let mut abandoned = renderer.encoder();
    renderer.try_draw(&mut abandoned,&target,None,&[quad.clone()]).unwrap();
    drop(abandoned);
    assert_eq!(knife_perf::get(Metric::PsOp7).delta(metric).calls,0);
    let mut failed = renderer.encoder();
    renderer.try_draw(&mut failed,&target,None,&[quad]).unwrap();
    control.borrow_mut().fail_next_draw = true;
    assert!(renderer.try_present(failed,&target).is_err());
    assert_eq!(knife_perf::get(Metric::PsOp7).delta(metric).calls,0);
    assert_eq!(executed_work_snapshot(),before);
}

#[test]
fn recorded_merged_draw_work_is_ready_without_executing_or_profiling() {
    let _options=Options::set(knife_perf::optimizations::PS_FETCH | knife_perf::optimizations::PS_PLAIN_MATERIAL);
    let gxm=Gxm::new().unwrap();let control=gxm.control.clone();
    control.borrow_mut().shader_capabilities=1u64<<30;
    let mut renderer=Renderer::new_with_gxm(gxm).unwrap();
    let target=renderer.create_target(100,50,"recorded work target");
    let q=Quad {shade:Shade::Fill,blend:Blend::Ps(PsOp::Add),
        ..Quad::rect(renderer.white.clone(),[0.,0.,50.,50.],[1.;4])};
    let before=executed_work_snapshot();
    let mut encoder=renderer.encoder();
    renderer.try_draw(&mut encoder,&target,None,&[q.clone(),q]).unwrap();
    let draws:Vec<_>=encoder.commands.iter().filter_map(|command|match command{
        Command::Draw{work,vertices,..}=>Some((*work,vertices.len())),_=>None,
    }).collect();
    assert_eq!(draws.len(),1);
    let (work,vertices)=draws[0];
    assert_eq!(vertices,12);
    assert_eq!((work.ops,work.flags,work.target_size,work.source_draws,work.pixels),([1,0],16,[100,50],2,5000));
    assert_eq!(executed_work_snapshot(),before);
    let _profile=Profiling::on();let before=executed_work_snapshot();let op=knife_perf::get(Metric::PsOp1);
    renderer.try_present(encoder,&target).unwrap();
    let after=executed_work_snapshot();
    assert_eq!((after.0-before.0,after.1-before.1),(1,5000));
    assert_eq!(knife_perf::get(Metric::PsOp1).delta(op).calls,1);
}

#[test]
fn source_prepare_failure_does_not_count_the_pending_draw() {
    let _options=Options::set(knife_perf::optimizations::PS_FETCH | knife_perf::optimizations::PS_PLAIN_MATERIAL);
    let gxm=Gxm::new().unwrap();let control=gxm.control.clone();control.borrow_mut().shader_capabilities=1u64<<31;
    let mut renderer=Renderer::new_with_gxm(gxm).unwrap();let target=renderer.create_target(8,8,"prepare failure");
    let q=Quad{blend:Blend::Ps(PsOp::Dodge),..Quad::rect(renderer.white.clone(),[0.,0.,8.,8.],[1.;4])};
    let _profile=Profiling::on();let before=executed_work_snapshot();let op=knife_perf::get(Metric::PsOp6);
    let mut encoder=renderer.encoder();renderer.try_draw(&mut encoder,&target,None,&[q]).unwrap();
    // Batch preparation precedes all draws and reports its allocation error.
    control.borrow_mut().fail_next_begin_batch=true;
    assert!(renderer.try_present(encoder,&target).is_err());
    assert_eq!(executed_work_snapshot(),before);assert_eq!(knife_perf::get(Metric::PsOp6).delta(op).calls,0);
}

#[test]
fn invalidated_group_cut_refreshes_recorded_physical_work_before_submission() {
    let _options=Options::set(knife_perf::optimizations::GROUP_BOUNDS);
    let gxm=Gxm::new().unwrap();let mut renderer=Renderer::new_with_gxm(gxm).unwrap();
    let target=renderer.create_target(16,16,"work group target");
    let pixel=renderer.create_target(16,16,"work group pixel");
    let mask=renderer.create_target(16,16,"work group mask");
    let mut init=renderer.encoder();
    for source in [&pixel,&mask]{renderer.try_draw(&mut init,source,Some([0.;4]),&[
        Quad::rect(renderer.white.clone(),[2.,2.,2.,2.],[1.;4]),
    ]).unwrap();}
    renderer.try_present(init,&target).unwrap();
    let mut encoder=renderer.encoder();renderer.try_group_composite(&mut encoder,&target,&pixel,&mask,Blend::Alpha,0.5).unwrap();
    let work=encoder.commands.iter().find_map(|command|match command{Command::Draw{work,..}=>Some(*work),_=>None}).unwrap();
    assert_eq!((work.pixels,work.role,work.target_size),(4,2,[16,16]));
    renderer.try_write_region(&pixel.tex,[12,12,1,1],4,&[255;4]).unwrap();
    coverage::before_submit(&mut encoder.commands,encoder.coverage_id);
    let work=encoder.commands.iter().find_map(|command|match command{Command::Draw{work,..}=>Some(*work),_=>None}).unwrap();
    assert_eq!((work.pixels,work.source_draws),(256,1));
}
