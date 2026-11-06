use super::*;
use super::test_backend::Event;
const ALL:u64=1u64<<38;
const PLAIN7:u64=1u64<<39;
struct Options(u64);
impl Options {fn set(mask:u64)->Self{let old=knife_perf::optimizations::mask();knife_perf::optimizations::set(mask);Self(old)}}
impl Drop for Options{fn drop(&mut self){knife_perf::optimizations::set(self.0);}}
const OPS:[PsOp;24]=[PsOp::Add,PsOp::Darken,PsOp::Lighten,PsOp::LinearBurn,PsOp::Burn,PsOp::Dodge,PsOp::DodgeAlpha,PsOp::Difference,PsOp::DifferenceAlpha,PsOp::Exclusion,PsOp::HardLight,PsOp::HardLightEx,PsOp::Overlay,PsOp::SoftLight,PsOp::Sub,PsOp::AddNoAlpha,PsOp::SubNoAlpha,PsOp::NoAlpha,PsOp::Normal,PsOp::DarkenColorAlpha,PsOp::LightenColorAlpha,PsOp::ScreenColorAlpha,PsOp::MultiplyDst,PsOp::Copy];
#[test]
fn group_material_routes_and_falls_back_without_changing_mask_contract() {
    let profile=knife_perf::optimizations::HARDWARE_TEST;
    let _scope=Options::set(profile);
    let (mut r,c)=setup();
    let target=r.create_target(960,540,"group destination");
    let pixel=r.create_target(960,540,"group pixel");
    let mask=r.create_target(960,540,"group mask");
    for (op,id) in [(6,105),(8,106),(14,107)] {
        for case in 0..5 {
            knife_perf::optimizations::set(if case==1 {profile & !knife_perf::optimizations::PS_GROUP_MATERIAL} else {profile});
            c.borrow_mut().shader_capabilities=if case==4 {0} else {1u64<<17};
            c.borrow_mut().shader_capabilities_high=if case>=3 {0} else {
                (1u64<<(op-1)) | if case==2 {0} else {1u64<<(id-64)}
            };
            c.borrow_mut().events.clear();
            let mut e=r.encoder();
            r.try_group_composite(&mut e,&target,&pixel,&mask,Blend::Ps(OPS[op-1]),0.37).unwrap();
            r.try_present(e,&target).unwrap();
            let trace=c.borrow();
            let actual=trace.events.iter().find_map(|event|match event {
                Event::Draw {shader,params,..} => Some((*shader as usize,*params)), _=>None,
            }).unwrap();
            assert_eq!(actual.0,match case {0=>id,1|2=>63+op,3=>17,_=>14});
            assert_eq!(actual.1,[op as f32,0.,960.,540.,-1.,0.,0.,1.]);
        }
    }
}
fn setup()->(Renderer,super::test_backend::Control){let g=Gxm::new().unwrap();let c=g.control.clone();let r=Renderer::new_with_gxm(g).unwrap();c.borrow_mut().events.clear();(r,c)}
#[test]
fn op7_pair_is_global_bounded_and_independently_disabled() {
 let profile=knife_perf::optimizations::HARDWARE_TEST;let _scope=Options::set(profile);
 let (mut r,c)=setup();let target=r.create_target(960,540,"op7 pair");
 for case in 0..10 {
  knife_perf::optimizations::set(if case==1 {profile & !knife_perf::optimizations::PS_CHAIN_OP7} else {profile});
  c.borrow_mut().shader_capabilities=(1u64<<17)|(1u64<<27)|(1u64<<48)|if case==2 {0} else {1u64<<57};
  c.borrow_mut().shader_capabilities_high=(1u64<<24)-1;
  let mut a=Quad {blend:Blend::Ps(PsOp::DodgeAlpha),..Quad::rect(r.white.clone(),[0.,0.,600.,540.],[0.3,0.6,0.8,0.4])};
  let mut b=Quad {blend:Blend::Ps(PsOp::DodgeAlpha),..Quad::rect(r.white.clone(),[200.,0.,600.,540.],[0.8,0.2,0.4,0.7])};
  if case==3 {a.shade=Shade::Fill;}
  if case==4 {b.shade=Shade::NoAlpha;}
  if case==5 {a.haze[0][3]=0.1;}
  if case==6 {b.blend=Blend::Ps(PsOp::Overlay);}
  if case==7 {b=Quad {blend:Blend::Ps(PsOp::DodgeAlpha),..Quad::rect(r.white.clone(),[596.,0.,100.,540.],[1.;4])};}
  if case==8 {a.clip=Some([[20.,20.],[580.,20.],[580.,520.],[20.,520.]]);}
  if case==9 {for p in &mut b.pos {p[0]=1000.-p[0];}}
  c.borrow_mut().events.clear();let mut e=r.encoder();
  r.try_draw(&mut e,&target,None,&[a,b]).unwrap();
  let fused=e.commands.iter().filter_map(|cmd|match cmd {Command::Draw {shader:EffectShader::PsChain2,work,..}=>Some(*work),_=>None}).collect::<Vec<_>>();
  let wanted=matches!(case,0|8|9);assert_eq!(!fused.is_empty(),wanted,"case={case}");
  if wanted {assert_eq!(fused[0].program,57);assert_eq!(fused[0].ops,[7,7]);assert!(fused[0].pixels>=8192);}
  r.try_present(e,&target).unwrap();
  assert!(!c.borrow().events.iter().any(|event|matches!(event,Event::Copy {..})));
 }
}
#[test]
fn plain_fp32_op7_prefers_exact_material_contract_and_keeps_fallbacks() {
 let profile=knife_perf::optimizations::PS_FETCH|ALL|PLAIN7|knife_perf::optimizations::HALF_COLOR;
 let _scope=Options::set(profile); let(mut r,c)=setup();
 let target=r.create_target(960,540,"plain fp32 op7");
 c.borrow_mut().shader_capabilities_high=1u64<<(70-64);
 for shade in [Shade::Sprite,Shade::Fill,Shade::NoAlpha] {
  for case in 0..6 {
   knife_perf::optimizations::set(if case==1 {profile & !PLAIN7} else {profile});
   c.borrow_mut().shader_capabilities=(1u64<<17)|(1u64<<51)|if case==2 {0} else {1u64<<48};
   let mut q=Quad {shade,blend:Blend::Ps(PsOp::DodgeAlpha),
    ..Quad::rect(r.white.clone(),[0.,0.,960.,540.],[0.3,0.6,0.8,0.4])};
   q.clip=Some([[1.,1.],[959.,1.],[959.,539.],[1.,539.]]);
   if case==3 {q.haze[1][3]=0.5;}
   if case==4 {q.rule=Some(RuleMask::new(r.white.clone(),0.3));}
   if case==5 {q.soft=Some(SoftClip {inner:[0.,0.,960.,540.],pivot:[480.,270.],rot:0.2,radius:1.,row:0,texels:4,fill:0.});}
   c.borrow_mut().events.clear();let mut e=r.encoder();
   r.try_draw(&mut e,&target,None,&[q]).unwrap();r.try_present(e,&target).unwrap();
   let events=&c.borrow().events;
   let (actual,params)=events.iter().find_map(|event|match event {Event::Draw{shader,params,..}=>Some((*shader as u32,*params)),_=>None}).unwrap();
   assert_eq!(actual,match case {0=>48,1|2=>51,_=>70},"shade={shade:?} case={case}");
   assert_eq!(params[0],7.);assert_eq!(params[1],if shade==Shade::Fill {1.} else {0.});
   assert_eq!(params[6],if shade==Shade::NoAlpha {1.} else {0.});
   assert!(!events.iter().any(|event|matches!(event,Event::Copy{..})));
  }
 }
}
#[test]
fn all_ps_operations_route_complete_materials_and_group_composites(){
 let _scope=Options::set(knife_perf::optimizations::PS_FETCH|ALL);
 let(mut r,c)=setup();let target=r.create_target(8,8,"all operation target");
 let pixel=r.create_target(8,8,"all operation pixel");let mask=r.create_target(8,8,"all operation mask");
 c.borrow_mut().shader_capabilities=1u64<<17;
 c.borrow_mut().shader_capabilities_high=(1u64<<24)-1;
 for (index,op) in OPS.into_iter().enumerate(){
  let mut q=Quad{shade:Shade::Fill,blend:Blend::Ps(op),..Quad::rect(r.white.clone(),[0.,0.,8.,8.],[0.3,0.6,0.8,0.4])};
  q.haze[0]=[0.2,0.4,0.6,0.8];q.rule=Some(RuleMask::new(r.white.clone(),0.3));
  q.soft=Some(SoftClip{inner:[0.,0.,8.,8.],pivot:[4.,4.],rot:0.2,radius:1.,row:0,texels:4,fill:0.});
  for group in [false,true]{
   c.borrow_mut().events.clear();let mut e=r.encoder();
   if group {r.try_group_composite(&mut e,&target,&pixel,&mask,Blend::Ps(op),0.7).unwrap();}
   else {r.try_draw(&mut e,&target,None,&[q.clone()]).unwrap();}
   r.try_present(e,&target).unwrap();let log=c.borrow();
   assert!(!log.events.iter().any(|e|matches!(e,Event::Copy{..})));
   let actual=log.events.iter().find_map(|e|match e{Event::Draw{shader,params,..}=>Some((*shader as u32,*params)),_=>None}).unwrap();
   assert_eq!(actual.0,64+index as u32,"op={} group={group}",op as u32);
   assert_eq!(actual.1[0],op as u32 as f32);assert_eq!(actual.1[7],if group{1.}else{0.});
  }
 }
}
#[test]
fn all_ps_high_bank_missing_capability_and_disabled_profile_keep_old_routes(){
 let _scope=Options::set(knife_perf::optimizations::PS_FETCH|ALL);
 let(mut r,c)=setup();let target=r.create_target(4,4,"bank fallback");
 let q=Quad{shade:Shade::Fill,blend:Blend::Ps(PsOp::Add),..Quad::rect(r.white.clone(),[0.,0.,4.,4.],[1.;4])};
 for(profile,lo,hi,expected)in[(16|ALL,1u64<<17,0,17),(16|ALL,0,0,14),(16,(1u64<<17),1,17),(ALL,1u64<<17,1,14)]{
  knife_perf::optimizations::set(profile);c.borrow_mut().shader_capabilities=lo;c.borrow_mut().shader_capabilities_high=hi;
  c.borrow_mut().events.clear();let mut e=r.encoder();r.try_draw(&mut e,&target,None,&[q.clone()]).unwrap();r.try_present(e,&target).unwrap();
  assert!(c.borrow().events.iter().any(|e|matches!(e,Event::Draw{shader,..}if *shader as u32==expected)));
 }
}

#[test]
fn hardware_half_plain_materials_use_real_programs_for_all_four_ops_and_shades() {
    let _scope = Options::set(knife_perf::optimizations::PS_FETCH | ALL
        | knife_perf::optimizations::PS_PLAIN_MATERIAL
        | knife_perf::optimizations::HALF_COLOR);
    let (mut r, c) = setup();
    let target = r.create_target(8, 8, "half hardware draw");
    c.borrow_mut().shader_capabilities = (15u64 << 49) | (1u64 << 17)
        | (1u64 << 21) | (1u64 << 30) | (1u64 << 31);
    c.borrow_mut().shader_capabilities_high = (1u64 << 24) - 1;
    for (op, id) in [(PsOp::Add,49u32),(PsOp::Dodge,50),(PsOp::DodgeAlpha,51),(PsOp::Overlay,52)] {
        for shade in [Shade::Sprite, Shade::Fill, Shade::NoAlpha] {
            let mut q = Quad { shade, blend: Blend::Ps(op),
                ..Quad::rect(r.white.clone(), [0.,0.,8.,8.], [0.3,0.6,0.8,0.4]) };
            q.clip = Some([[1.,1.],[7.,1.],[7.,7.],[1.,7.]]);
            c.borrow_mut().events.clear();
            let mut e = r.encoder();
            r.try_draw(&mut e, &target, None, &[q]).unwrap();
            r.try_present(e, &target).unwrap();
            let trace = c.borrow();
            let (actual, params) = trace.events.iter().find_map(|event| match event {
                Event::Draw { shader, params, .. } => Some((*shader as u32, *params)), _ => None,
            }).unwrap();
            assert_eq!(actual, id, "op={op:?} shade={shade:?} must execute Half candidate");
            assert_eq!(params[0], op as u32 as f32);
            assert_eq!(params[1], if shade == Shade::Fill {1.} else {0.});
            assert_eq!(params[6], if shade == Shade::NoAlpha {1.} else {0.});
            assert!(!trace.events.iter().any(|event| matches!(event, Event::Copy { .. })));
        }
    }
}

#[test]
fn hardware_half_respects_material_gate_missing_program_and_explicit_disable() {
    let full_profile = knife_perf::optimizations::PS_FETCH | ALL
        | knife_perf::optimizations::PS_PLAIN_MATERIAL | knife_perf::optimizations::HALF_COLOR;
    let _scope = Options::set(full_profile);
    let (mut r, c) = setup();
    let target = r.create_target(8,8,"half fallback");
    c.borrow_mut().shader_capabilities_high = (1u64 << 24) - 1;
    for (op,half,normal,full) in [(PsOp::Add,49u32,30u32,64u32),
        (PsOp::Dodge,50,31,69),(PsOp::DodgeAlpha,51,70,70),(PsOp::Overlay,52,21,76)] {
        for case in 0..5 {
            knife_perf::optimizations::set(if case==1 {
                full_profile & !knife_perf::optimizations::HALF_COLOR
            } else {full_profile});
            c.borrow_mut().shader_capabilities = (1u64<<17)|(1u64<<21)|(1u64<<30)|(1u64<<31)
                | if case==0 {0} else {1u64<<half};
            let mut q=Quad {blend:Blend::Ps(op),..Quad::rect(r.white.clone(),[0.,0.,8.,8.],[1.;4])};
            if case==2 {q.haze[0][3]=0.5;}
            if case==3 {q.rule=Some(RuleMask::new(r.white.clone(),0.3));}
            if case==4 {q.soft=Some(SoftClip {inner:[0.,0.,8.,8.],pivot:[4.,4.],rot:0.2,
                radius:1.,row:0,texels:4,fill:0.});}
            c.borrow_mut().events.clear();
            let mut e=r.encoder();r.try_draw(&mut e,&target,None,&[q]).unwrap();
            r.try_present(e,&target).unwrap();
            let actual=c.borrow().events.iter().find_map(|event|match event {
                Event::Draw {shader,..}=>Some(*shader as u32),_=>None,
            }).unwrap();
            assert_eq!(actual,if case<2 {normal} else {full},"op={op:?} case={case}");
        }
    }
}

#[test]
fn half_overlay_keeps_one_precision_when_blend_chain_is_also_enabled() {
    let _scope = Options::set(knife_perf::optimizations::HARDWARE_TEST);
    let (mut r, c) = setup();
    let target = r.create_target(960, 540, "half chain interaction");
    c.borrow_mut().shader_capabilities = (1u64 << 52) | (1u64 << 21)
        | (1u64 << 17) | (1u64 << 27);
    let a = Quad { blend: Blend::Ps(PsOp::Overlay),
        ..Quad::rect(r.white.clone(), [0., 0., 600., 540.], [0.3, 0.6, 0.8, 0.4]) };
    let b = Quad { blend: Blend::Ps(PsOp::Overlay),
        ..Quad::rect(r.white.clone(), [200., 0., 600., 540.], [0.8, 0.2, 0.4, 0.7]) };
    // This geometry is deliberately large enough for chaining, with visible
    // intersection and outside regions on each sprite. FP32 remains eligible.
    assert!(super::blend_chain::pair(&a, &b,
        [EffectShader::PsFetch13; 2], [960., 540.], [960, 540],
        &r.white.native, &r.soft_lut).is_some());
    for shaders in [[EffectShader::PsHalf13; 2],
        [EffectShader::PsHalf13, EffectShader::PsFetch13],
        [EffectShader::PsFetch13, EffectShader::PsHalf13]] {
        assert!(super::blend_chain::pair(&a, &b, shaders,
            [960., 540.], [960, 540], &r.white.native, &r.soft_lut).is_none(),
            "Half arithmetic cannot be replaced by an FP32 chain intersection");
    }
    c.borrow_mut().events.clear();
    let mut e = r.encoder();
    r.try_draw(&mut e, &target, None, &[a, b]).unwrap();
    r.try_present(e, &target).unwrap();
    let trace = c.borrow();
    let draws: Vec<_> = trace.events.iter().filter_map(|event| match event {
        Event::Draw { shader, vertices, .. } => Some((*shader, vertices.len())), _ => None,
    }).collect();
    // Ordinary same-program vertex batching may combine both draws without
    // replacing their arithmetic by a different intersection shader.
    assert!(!draws.is_empty());
    assert!(draws.iter().all(|(shader, _)| *shader == EffectShader::PsHalf13));
    assert_eq!(draws.iter().map(|(_, count)| *count).sum::<usize>(), 12);
}


#[test]
fn material_variants_route_globally_and_keep_per_entry_fallback() {
 let profile=knife_perf::optimizations::HARDWARE_TEST;let _scope=Options::set(profile);
 let(mut r,c)=setup();let target=r.create_target(960,540,"material candidates");
 let operations=[1,1,6,6,7,7,13,13,1,6,7,13,3,4,5,11,14];
 let bases=[49,49,50,50,48,48,52,52,64,69,70,76,66,67,68,74,77];
 for index in 0..17 {
  let op=OPS[operations[index]-1];let id=88+index as u32;
  for variant in 0..6 {
   knife_perf::optimizations::set(if variant==2 {profile & !knife_perf::optimizations::PS_MATERIAL_VARIANTS}else{profile});
   c.borrow_mut().shader_capabilities=(1u64<<17)|(1u64<<48)|(15u64<<49);
   c.borrow_mut().shader_capabilities_high=((1u64<<24)-1)|if variant==1 {0}else{1u64<<(id-64)};
   let shade=if index<8 {if index%2==0 {Shade::Fill}else{Shade::NoAlpha}}else{Shade::Fill};
   let mut q=Quad{shade,blend:Blend::Ps(op),..Quad::rect(r.white.clone(),[0.,0.,960.,540.],[0.3,0.6,0.8,0.4])};
   if (8..12).contains(&index){q.haze[0]=[0.01,-0.5,0.7,1.];q.haze[1]=[-0.01,0.2,-0.5,0.];}
   q.clip=Some([[1.,1.],[959.,1.],[959.,539.],[1.,539.]]);
   if variant==3 {q.rule=Some(RuleMask::new(r.white.clone(),0.3));}
   if variant==4 {q.soft=Some(SoftClip{inner:[0.,0.,960.,540.],pivot:[480.,270.],rot:0.2,radius:1.,row:0,texels:4,fill:0.});}
   if variant==5 {q.masked=true;}
   c.borrow_mut().events.clear();let mut e=r.encoder();r.try_draw(&mut e,&target,None,&[q]).unwrap();
   r.try_present(e,&target).unwrap();
   let actual=c.borrow().events.iter().find_map(|event|match event {Event::Draw{shader,..}=>Some(*shader as u32),_=>None}).unwrap();
   assert_eq!(actual,if variant==0 {id}else if variant<=2 {bases[index]}else{63+operations[index] as u32},"id={id} variant={variant}");
  }
 }
}
