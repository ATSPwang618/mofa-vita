use super::*;
use super::test_backend::Event;

const FULL: u64 = 1u64 << 37;
struct Options(u64);
impl Options {
    fn set(mask:u64)->Self {
        let previous=knife_perf::optimizations::mask();
        knife_perf::optimizations::set(mask); Self(previous)
    }
}
impl Drop for Options {
    fn drop(&mut self) {knife_perf::optimizations::set(self.0);}
}
fn setup()->(Renderer,super::test_backend::Control) {
    let gxm=Gxm::new().unwrap();let control=gxm.control.clone();
    let renderer=Renderer::new_with_gxm(gxm).unwrap();
    control.borrow_mut().events.clear();(renderer,control)
}

#[test]
fn full_ps_routes_all_three_ops_with_effect_inputs_and_preserves_payload() {
    let _scope = Options::set(knife_perf::optimizations::PS_FETCH | FULL);
    let (mut r, control) = setup();
    let target = r.create_target(8, 8, "full PS material");
    for (op, id) in [(PsOp::Add,54u32),(PsOp::Dodge,55),(PsOp::Overlay,56)] {
        for shade in [Shade::Sprite,Shade::Fill,Shade::NoAlpha] {
            let mut q = Quad { shade, blend: Blend::Ps(op),
                ..Quad::rect(r.white.clone(), [0.,0.,8.,8.], [0.3,0.6,0.8,0.4]) };
            q.haze[0] = [0.2,0.4,0.6,0.8];
            q.rule = Some(RuleMask::new(r.white.clone(),0.3));
            q.soft = Some(SoftClip { inner:[0.,0.,8.,8.],pivot:[4.,4.],rot:0.2,
                radius:1.,row:0,texels:4,fill:0. });
            q.clip = Some([[1.,1.],[7.,1.],[7.,7.],[1.,7.]]);
            let mut recorded = Vec::new();
            for caps in [1u64 << 17, (1u64 << 17) | (1u64 << id)] {
                control.borrow_mut().shader_capabilities = caps;
                control.borrow_mut().events.clear();
                let mut enc = r.encoder();
                r.try_draw(&mut enc,&target,None,&[q.clone()]).unwrap();
                r.try_present(enc,&target).unwrap();
                let trace = control.borrow();
                assert!(!trace.events.iter().any(|e|matches!(e,Event::Copy{..})));
                recorded.push(trace.events.iter().find_map(|e|match e {
                    Event::Draw { shader,params,vertices,textures,.. } =>
                        Some((*shader as u32,*params,vertices.clone(),*textures)),_=>None,
                }).unwrap());
            }
            assert_eq!(recorded[0].0,17);
            assert_eq!(recorded[1].0,id,"full material must leave generic dispatch");
            assert_eq!(recorded[0].1,recorded[1].1);
            assert_eq!(recorded[0].3,recorded[1].3);
            assert_eq!(format!("{:?}",recorded[0].2),format!("{:?}",recorded[1].2));
        }
    }
}

#[test]
fn full_ps_group_composites_use_complete_source_and_keep_mask_sampling() {
    let _scope = Options::set(knife_perf::optimizations::PS_FETCH | FULL);
    let (mut r,control) = setup();
    let target = r.create_target(4,4,"full PS group target");
    let pixel = r.create_target(4,4,"full PS group pixel");
    let mask = r.create_target(4,4,"full PS group mask");
    for (op,id) in [(PsOp::Add,54u32),(PsOp::Dodge,55),(PsOp::Overlay,56)] {
        control.borrow_mut().shader_capabilities = (1u64<<17)|(1u64<<id);
        control.borrow_mut().events.clear();
        let mut enc = r.encoder();
        r.try_group_composite(&mut enc,&target,&pixel,&mask,Blend::Ps(op),0.7).unwrap();
        r.try_present(enc,&target).unwrap();
        let trace=control.borrow();
        assert!(!trace.events.iter().any(|e|matches!(e,Event::Copy{..})));
        let (shader,params,textures)=trace.events.iter().find_map(|e|match e {
            Event::Draw {shader,params,textures,..}=>Some((*shader as u32,*params,*textures)),_=>None,
        }).unwrap();
        assert_eq!(shader,id);
        assert_eq!(params,[op as u32 as f32,0.,4.,4.,-1.,0.,0.,1.]);
        assert_ne!(textures[0],textures[2],"group mask must remain a separate sample");
    }
}

#[test]
fn full_ps_missing_capabilities_fall_back_and_disabled_profile_keeps_baseline() {
    let _scope=Options::set(knife_perf::optimizations::PS_FETCH | FULL);
    let (mut r,control)=setup();
    let target=r.create_target(4,4,"full fallback");
    for op in [PsOp::Add,PsOp::Dodge,PsOp::Overlay] {
        let mut q=Quad {blend:Blend::Ps(op),..Quad::rect(r.white.clone(),[0.,0.,4.,4.],[1.;4])};
        q.haze[1][2]=0.4;
        for (profile,caps,expected,copies) in [
            (knife_perf::optimizations::PS_FETCH|FULL,1u64<<17,17,0),
            (knife_perf::optimizations::PS_FETCH|FULL,0,14,1),
            (knife_perf::optimizations::PS_FETCH,(1u64<<17)|(7u64<<54),17,0),
            (FULL,(1u64<<17)|(7u64<<54),14,1),
        ] {
            knife_perf::optimizations::set(profile);
            control.borrow_mut().shader_capabilities=caps;
            control.borrow_mut().events.clear();
            let mut enc=r.encoder();
            r.try_draw(&mut enc,&target,None,&[q.clone()]).unwrap();
            r.try_present(enc,&target).unwrap();
            let trace=control.borrow();
            assert_eq!(trace.events.iter().filter(|e|matches!(e,Event::Copy{..})).count(),copies);
            assert!(trace.events.iter().any(|e|matches!(e,Event::Draw{shader,..} if *shader as u32==expected)));
        }
    }
}

#[test]
fn full_ps_keeps_existing_plain_programs_first_and_does_not_capture_other_ops() {
    let _scope=Options::set(knife_perf::optimizations::PS_FETCH
        | knife_perf::optimizations::PS_PLAIN_MATERIAL | FULL);
    let (mut r,control)=setup();
    let target=r.create_target(4,4,"plain precedence");
    control.borrow_mut().shader_capabilities=(1u64<<17)|(1u64<<21)|(1u64<<30)|(1u64<<31)|(7u64<<54);
    for (op,id) in [(PsOp::Add,30u32),(PsOp::Dodge,31),(PsOp::Overlay,21),(PsOp::Difference,17)] {
        control.borrow_mut().events.clear();
        let q=Quad {blend:Blend::Ps(op),..Quad::rect(r.white.clone(),[0.,0.,4.,4.],[1.;4])};
        let mut enc=r.encoder();r.try_draw(&mut enc,&target,None,&[q]).unwrap();
        r.try_present(enc,&target).unwrap();
        assert!(control.borrow().events.iter().any(|e|matches!(e,Event::Draw{shader,..} if *shader as u32==id)));
    }
}

#[test]
fn full_ps_group_missing_entry_uses_generic_then_snapshot() {
    let _scope=Options::set(knife_perf::optimizations::PS_FETCH|FULL);
    let (mut r,control)=setup();
    let target=r.create_target(4,4,"group fallback target");
    let pixel=r.create_target(4,4,"group fallback pixel");
    let mask=r.create_target(4,4,"group fallback mask");
    for op in [PsOp::Add,PsOp::Dodge,PsOp::Overlay] {
        for (caps,expected,copies) in [(1u64<<17,17u32,0),(0,14,1)] {
            control.borrow_mut().shader_capabilities=caps;
            control.borrow_mut().events.clear();
            let mut enc=r.encoder();
            r.try_group_composite(&mut enc,&target,&pixel,&mask,Blend::Ps(op),0.4).unwrap();
            r.try_present(enc,&target).unwrap();
            let trace=control.borrow();
            assert!(trace.events.iter().any(|e|matches!(e,Event::Draw{shader,..} if *shader as u32==expected)));
            assert_eq!(trace.events.iter().filter(|e|matches!(e,Event::Copy{..})).count(),copies);
        }
    }
}
